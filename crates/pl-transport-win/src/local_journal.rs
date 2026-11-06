// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The way this app reaches a journal running on this computer, in one place.
//!
//! Today that is unauthenticated HTTP to the journal's loopback port, the same
//! routes the journal's own window and pages call. Nothing read here is trusted
//! for more than an offer: the connect verifies the journal over the pinned,
//! authenticated connection in [`crate::session::connect_found_journal`]. This
//! module sends the journal nothing of the owner's beyond the device label.

use std::path::{Path, PathBuf};
use std::time::Duration;

use observer_model::MarkRenderSpec;
use serde::Serialize;

use crate::client::{accept_link_identity, parse_link_identity, LinkIdentity};
use crate::TransportError;

/// The port a journal listens on unless it publishes another.
pub const DEFAULT_JOURNAL_PORT: u16 = 5015;

const PAIR_START_PATH: &str = "/app/network/pair-start";
const READ_BOUND: Duration = Duration::from_secs(3);
const MINT_BOUND: Duration = Duration::from_secs(10);
const RESPONSE_CAP: u64 = 256 * 1024;

/// A committed journal answering on this computer: what the offer shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FoundJournal {
    pub instance_id: String,
    pub mark: MarkRenderSpec,
}

/// A same-computer link minted for one connect press. Never logged or saved.
#[derive(Clone, PartialEq, Eq)]
pub struct MintedLink {
    pub pair_link: String,
    pub ca_fingerprint: String,
}

pub struct LocalJournalDoor {
    base: String,
    agent: ureq::Agent,
}

impl LocalJournalDoor {
    pub fn at_port(port: u16) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .proxy(None)
            .max_redirects(0)
            .max_idle_connections(0)
            .http_status_as_error(false)
            .timeout_global(Some(READ_BOUND))
            .build()
            .into();
        Self {
            base: format!("http://127.0.0.1:{port}"),
            agent,
        }
    }

    /// The committed journal answering here, or `None` for anything else: nothing
    /// listening, a journal still in first run, or something that isn't a journal.
    pub fn identify(&self) -> Option<FoundJournal> {
        let identity = self.read_identity().ok()?;
        let instance_id = identity.instance_id.clone()?;
        let mark = accept_link_identity(&identity, &instance_id).ok()?;
        Some(FoundJournal { instance_id, mark })
    }

    /// Have the journal mint a same-computer link, while it stays the instance the
    /// owner was offered on both sides of the mint.
    pub fn mint_link(
        &self,
        expected_instance_id: &str,
        device_label: &str,
    ) -> Result<MintedLink, TransportError> {
        self.require_instance(expected_instance_id)?;
        let minted = self.post_pair_start(device_label)?;
        self.require_instance(expected_instance_id)?;
        Ok(minted)
    }

    fn require_instance(&self, expected_instance_id: &str) -> Result<(), TransportError> {
        accept_link_identity(&self.read_identity()?, expected_instance_id).map(|_| ())
    }

    fn read_identity(&self) -> Result<LinkIdentity, TransportError> {
        let url = format!("{}{}", self.base, observer_contract::IDENTITY_PATH);
        let response = self
            .agent
            .get(&url)
            .header("Accept", "application/json")
            .call();
        parse_link_identity(&success_body(response)?)
    }

    fn post_pair_start(&self, device_label: &str) -> Result<MintedLink, TransportError> {
        let url = format!("{}{PAIR_START_PATH}", self.base);
        let body = serde_json::json!({ "same_machine": true, "device_label": device_label });
        let response = self
            .agent
            .post(&url)
            .config()
            .timeout_global(Some(MINT_BOUND))
            .build()
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(body.to_string());
        let answer: serde_json::Value = serde_json::from_slice(&success_body(response)?)?;
        let field = |name: &str| {
            answer
                .get(name)
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
                .ok_or_else(|| TransportError::Pairing(format!("pair-start answered no {name}")))
        };
        Ok(MintedLink {
            pair_link: field("pair_link")?,
            ca_fingerprint: field("ca_fingerprint")?,
        })
    }
}

fn success_body(
    response: Result<ureq::http::Response<ureq::Body>, ureq::Error>,
) -> Result<Vec<u8>, TransportError> {
    let mut response = response.map_err(door_error)?;
    let status = response.status().as_u16();
    if !(200..300).contains(&status) {
        return Err(TransportError::Pairing(format!(
            "local journal answered {status}"
        )));
    }
    response
        .body_mut()
        .with_config()
        .limit(RESPONSE_CAP)
        .read_to_vec()
        .map_err(door_error)
}

fn door_error(error: ureq::Error) -> TransportError {
    TransportError::Io(std::io::Error::other(error.to_string()))
}

/// The port the journal under this home directory publishes, or the default.
/// A journal's setup records its folder in the user config, and a running journal
/// records its port in that folder. Either can be absent; the identity check
/// decides what the answer on the port is.
pub fn published_port(home: &Path) -> u16 {
    read_published_port(home).unwrap_or(DEFAULT_JOURNAL_PORT)
}

fn read_published_port(home: &Path) -> Option<u16> {
    let journal = journal_folder(home);
    let text = std::fs::read_to_string(journal.join("health").join("convey.port")).ok()?;
    text.trim().parse().ok().filter(|port| *port != 0)
}

fn journal_folder(home: &Path) -> PathBuf {
    std::fs::read_to_string(home.join(".config").join("solstone").join("config.toml"))
        .ok()
        .and_then(|config| journal_from_config(&config))
        .unwrap_or_else(|| home.join("journal"))
}

/// Reads the one line the journal's setup writes: `journal = "<escaped path>"`.
fn journal_from_config(config: &str) -> Option<PathBuf> {
    config.lines().find_map(|line| {
        let (key, value) = line.split_once('=')?;
        if key.trim() != "journal" {
            return None;
        }
        let quoted = value.trim().strip_prefix('"')?.strip_suffix('"')?;
        let mut path = String::with_capacity(quoted.len());
        let mut chars = quoted.chars();
        while let Some(c) = chars.next() {
            if c == '\\' {
                match chars.next()? {
                    escaped @ ('\\' | '"') => path.push(escaped),
                    _ => return None,
                }
            } else {
                path.push(c);
            }
        }
        (!path.is_empty()).then(|| PathBuf::from(path))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_published_port_is_read_through_the_setup_config() {
        let home = tempfile::tempdir().unwrap();
        let journal = home.path().join(r#"a "quoted" \ folder"#);
        std::fs::create_dir_all(journal.join("health")).unwrap();
        std::fs::write(journal.join("health").join("convey.port"), "5016\n").unwrap();
        let config = home.path().join(".config").join("solstone");
        std::fs::create_dir_all(&config).unwrap();
        let escaped = journal
            .display()
            .to_string()
            .replace('\\', "\\\\")
            .replace('"', "\\\"");
        std::fs::write(
            config.join("config.toml"),
            format!("journal = \"{escaped}\"\n"),
        )
        .unwrap();
        assert_eq!(published_port(home.path()), 5016);
    }

    #[test]
    fn the_default_folder_and_the_default_port_stand_in_for_what_is_missing() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(published_port(home.path()), DEFAULT_JOURNAL_PORT);
        let health = home.path().join("journal").join("health");
        std::fs::create_dir_all(&health).unwrap();
        std::fs::write(health.join("convey.port"), "5017").unwrap();
        assert_eq!(published_port(home.path()), 5017);
        std::fs::write(health.join("convey.port"), "not a port").unwrap();
        assert_eq!(published_port(home.path()), DEFAULT_JOURNAL_PORT);
    }
}

#[cfg(all(test, feature = "transport-tests"))]
mod door_tests {
    use super::*;
    use crate::unknown_journals::mark_spec_for_jid;
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::sync::{Arc, Mutex};

    const INSTANCE: &str = "f30ed159-ef46-8e9c-913f-e49f0fe7d201";
    const OTHER_INSTANCE: &str = "5dda9388-857e-41a7-b89e-dd990a9ee026";

    struct Fixture {
        port: u16,
        requests: Arc<Mutex<Vec<String>>>,
    }

    /// Answers each request in turn with `(status, head extras, body)`.
    fn serve(answers: Vec<(u16, String)>) -> Fixture {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let seen = requests.clone();
        std::thread::spawn(move || {
            for (status, body) in answers {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = Vec::new();
                let mut chunk = [0u8; 4096];
                loop {
                    let n = stream.read(&mut chunk).unwrap_or(0);
                    request.extend_from_slice(&chunk[..n]);
                    let text = String::from_utf8_lossy(&request).to_string();
                    let complete = text.find("\r\n\r\n").is_some_and(|end| {
                        let length = text[..end]
                            .lines()
                            .find_map(|line| {
                                let (name, value) = line.split_once(':')?;
                                name.eq_ignore_ascii_case("content-length")
                                    .then(|| value.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        request.len() >= end + 4 + length
                    });
                    if n == 0 || complete {
                        break;
                    }
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&request).to_string());
                let head = format!(
                    "HTTP/1.1 {status} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(body.as_bytes());
            }
        });
        Fixture { port, requests }
    }

    fn identity_body(instance: &str) -> String {
        serde_json::json!({
            "committed": true,
            "instance_id": instance,
            "mark": mark_spec_for_jid(instance),
        })
        .to_string()
    }

    fn minted_body() -> String {
        serde_json::json!({
            "pair_link": "https://go.solstone.app/p#fixture",
            "ca_fingerprint": "ab".repeat(32),
        })
        .to_string()
    }

    #[test]
    fn a_committed_journal_is_found_with_its_own_mark() {
        let fixture = serve(vec![(200, identity_body(INSTANCE))]);
        let found = LocalJournalDoor::at_port(fixture.port).identify().unwrap();
        assert_eq!(found.instance_id, INSTANCE);
        assert_eq!(Some(found.mark), mark_spec_for_jid(INSTANCE));
    }

    #[test]
    fn anything_but_a_committed_journal_gets_no_offer() {
        let mut wrong_mark =
            serde_json::from_str::<serde_json::Value>(&identity_body(INSTANCE)).unwrap();
        wrong_mark["mark"]["words"][0] = "wrong".into();
        let cases = [
            (200, serde_json::json!({ "committed": false }).to_string()),
            (
                200,
                serde_json::json!({ "committed": false, "instance_id": INSTANCE, "mark": mark_spec_for_jid(INSTANCE) })
                    .to_string(),
            ),
            (200, wrong_mark.to_string()),
            (200, "<html>first run</html>".to_string()),
            (200, "{}".to_string()),
            (302, String::new()),
            (404, identity_body(INSTANCE)),
        ];
        for (status, body) in cases {
            let fixture = serve(vec![(status, body.clone())]);
            assert_eq!(
                LocalJournalDoor::at_port(fixture.port).identify(),
                None,
                "{status} {body}"
            );
        }
    }

    #[test]
    fn nothing_listening_gets_no_offer() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        assert_eq!(LocalJournalDoor::at_port(port).identify(), None);
    }

    #[test]
    fn a_mint_asks_for_a_same_computer_link_for_this_device() {
        let fixture = serve(vec![
            (200, identity_body(INSTANCE)),
            (200, minted_body()),
            (200, identity_body(INSTANCE)),
        ]);
        let minted = LocalJournalDoor::at_port(fixture.port)
            .mint_link(INSTANCE, "this pc")
            .unwrap();
        assert_eq!(minted.ca_fingerprint, "ab".repeat(32));
        let requests = fixture.requests.lock().unwrap();
        assert!(requests[1].starts_with("POST /app/network/pair-start "));
        let (_, body) = requests[1].split_once("\r\n\r\n").unwrap();
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["same_machine"], true);
        assert_eq!(body["device_label"], "this pc");
    }

    #[test]
    fn a_different_journal_is_never_asked_to_mint() {
        let fixture = serve(vec![(200, identity_body(OTHER_INSTANCE))]);
        assert!(LocalJournalDoor::at_port(fixture.port)
            .mint_link(INSTANCE, "this pc")
            .is_err());
        assert_eq!(fixture.requests.lock().unwrap().len(), 1);
    }

    #[test]
    fn a_journal_that_changed_during_the_mint_is_refused() {
        let fixture = serve(vec![
            (200, identity_body(INSTANCE)),
            (200, minted_body()),
            (200, identity_body(OTHER_INSTANCE)),
        ]);
        assert!(LocalJournalDoor::at_port(fixture.port)
            .mint_link(INSTANCE, "this pc")
            .is_err());
    }

    #[test]
    fn a_refused_mint_is_an_error() {
        let fixture = serve(vec![
            (200, identity_body(INSTANCE)),
            (409, serde_json::json!({"reason_code": "x"}).to_string()),
        ]);
        assert!(LocalJournalDoor::at_port(fixture.port)
            .mint_link(INSTANCE, "this pc")
            .is_err());
    }
}
