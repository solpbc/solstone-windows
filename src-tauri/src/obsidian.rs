// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The Obsidian vault source, wired into the app: the paired upload path as the
//! source's journal seam, and the commands the Settings window drives it with.
//!
//! Everything scheduled, scanned or held lives in `obsidian-source`. What is
//! here carries no journal identity into anything it holds.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use observer_model::{LocalOffset, LocalZone, PauseReason};
use obsidian_source::{
    Answer, BoxFuture, Capability, Delivery, DeliveryResult, FoundVault, Journal, ListedVault,
    ObsidianDocument, ObsidianHandle, VaultList, CAPABILITY, SOURCE,
};
use pl_transport_win::source_upload::{upload_source_file_for_reply, SourceFile, SourceFileReply};
use pl_transport_win::{ObserverClient, TransportError};
use tauri::Manager;

static HANDLE: OnceLock<ObsidianHandle> = OnceLock::new();

const FILE_CONTENT_TYPE: &str = "application/jsonl";
const CHOOSER_TITLE: &str = "choose your Obsidian vault";

type LoadClient = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Option<Arc<ObserverClient>>> + Send>> + Send + Sync,
>;
type LoadZone = Arc<dyn Fn() -> Option<LocalZone> + Send + Sync>;
type Heard = Arc<dyn Fn() + Send + Sync>;

/// The journal, over the app's paired upload path.
pub(crate) struct ClientJournal {
    load_client: LoadClient,
    load_zone: LoadZone,
    heard: Heard,
    asked: Mutex<Option<Arc<ObserverClient>>>,
}

impl ClientJournal {
    /// `heard` runs whenever the journal answers, a delivery or the capability
    /// check, so a PC sending only notes still shows when it last reached it.
    pub(crate) fn new(load_client: LoadClient, load_zone: LoadZone, heard: Heard) -> Self {
        Self {
            load_client,
            load_zone,
            heard,
            asked: Mutex::new(None),
        }
    }
}

impl Journal for ClientJournal {
    fn capability(&self) -> BoxFuture<'_, Capability> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                *self.asked.lock().unwrap_or_else(|p| p.into_inner()) = None;
                return Capability::Unknown;
            };
            *self.asked.lock().unwrap_or_else(|p| p.into_inner()) = Some(Arc::clone(&client));
            let answer = client.system_capabilities().await;
            if matches!(answer, Ok(_) | Err(TransportError::Rejected { .. })) {
                (self.heard)();
            }
            match answer {
                Ok(listed) if listed.iter().any(|name| name == CAPABILITY) => Capability::Supported,
                Ok(_) => Capability::NotListed,
                Err(_) => Capability::Unknown,
            }
        })
    }

    fn vault_list(&self) -> BoxFuture<'_, VaultList> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                return VaultList::Unavailable;
            };
            match client.obsidian_vaults().await {
                Ok(response) if response.status == 200 => parse_vault_list(&response.body)
                    .map_or(VaultList::Unavailable, VaultList::Listed),
                // An older journal without the route: no vault is known from elsewhere.
                Ok(response) if response.status == 404 => VaultList::Listed(Vec::new()),
                _ => VaultList::Unavailable,
            }
        })
    }

    fn deliver<'a>(&'a self, delivery: &'a Delivery) -> BoxFuture<'a, DeliveryResult> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                return DeliveryResult::NoAnswer;
            };
            let Some(zone) = (self.load_zone)() else {
                return DeliveryResult::NoAnswer;
            };
            let path = delivery.path.clone();
            let bytes = match tokio::task::spawn_blocking(move || std::fs::read(path)).await {
                Ok(Ok(bytes)) => bytes,
                Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
                    return DeliveryResult::FileMissing;
                }
                _ => return DeliveryResult::NoAnswer,
            };
            let reply = upload_source_file_for_reply(
                &client,
                SourceFile {
                    source: SOURCE,
                    day: &delivery.window.day,
                    segment: &delivery.window.segment,
                    zone: &zone,
                    filename: &delivery.name,
                    content_type: FILE_CONTENT_TYPE,
                    bytes,
                },
            )
            .await;
            if matches!(reply, SourceFileReply::Answered { .. }) {
                (self.heard)();
            }
            answer_for(reply)
        })
    }

    fn pairing_unchanged(&self) -> BoxFuture<'_, bool> {
        Box::pin(async move {
            let asked = self.asked.lock().unwrap_or_else(|p| p.into_inner()).clone();
            match (asked, (self.load_client)().await) {
                (Some(asked), Some(current)) => Arc::ptr_eq(&asked, &current),
                (None, None) => true,
                _ => false,
            }
        })
    }
}

/// How the app takes what the upload path got back. No answer means the owner
/// hasn't confirmed the journal's mark.
fn answer_for(reply: SourceFileReply) -> DeliveryResult {
    match reply {
        SourceFileReply::Answered {
            status,
            reason_code,
        } => DeliveryResult::Answer(Answer::from_reply(
            status.as_deref(),
            reason_code.as_deref(),
        )),
        SourceFileReply::Held => DeliveryResult::NoAnswer,
        SourceFileReply::Unreachable => DeliveryResult::Answer(Answer::Retry),
    }
}

pub(crate) fn parse_vault_list(body: &[u8]) -> Option<Vec<ListedVault>> {
    #[derive(serde::Deserialize)]
    struct Listing {
        vaults: Vec<ListedVault>,
    }
    serde_json::from_slice::<Listing>(body)
        .ok()
        .map(|listing| listing.vaults)
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn local_zone() -> Option<LocalZone> {
    platform_win::WindowsLocalOffset.local_zone(now_secs()).ok()
}

/// Open the source and start its work, off the setup path.
pub fn start(app: tauri::AppHandle, counters: Option<source_counters::SharedCounters>) {
    let client_app = app.clone();
    let load_client: LoadClient = Arc::new(move || {
        let app = client_app.clone();
        Box::pin(async move {
            let state = app.state::<crate::app::AppState>();
            pl_transport_win::access::load_current_client(&state.credential_access).await
        })
    });
    let heard_app = app.clone();
    let paused_app = app;
    // Only the owner's pause holds delivery. A locked screen or a suspending PC
    // pauses capture, and has no bearing on notes already written.
    let paused = Arc::new(move || {
        let state = paused_app.state::<crate::app::AppState>();
        state
            .health
            .lock()
            .map(|h| matches!(h.pause, Some(p) if p.reason == PauseReason::Operator))
            .unwrap_or(false)
    });
    let dir_ids: Arc<obsidian_source::ReadDirFileIds> =
        Arc::new(|dir: &Path| platform_win::dir_file_ids::file_ids_in_dir(dir));
    let heard: Heard = Arc::new(move || {
        let state = heard_app.state::<crate::app::AppState>();
        if let Ok(mut sync) = state.sync.lock() {
            sync.upload.record_answer(now_millis());
        };
    });
    let journal = ClientJournal::new(load_client, Arc::new(local_zone), heard);
    let root = platform_win::local_data_root().join("obsidian");
    let (handle, work) = obsidian_source::start(root, Some(dir_ids), journal, paused, counters);
    let _ = HANDLE.set(handle);
    tauri::async_runtime::spawn(work);
}

/// Whether the source is on, and when that changes.
pub fn on_changes() -> Option<tokio::sync::watch::Receiver<bool>> {
    handle().map(ObsidianHandle::on_changes)
}

fn handle() -> Option<&'static ObsidianHandle> {
    HANDLE.get()
}

fn app_data() -> Option<std::path::PathBuf> {
    std::env::var_os("APPDATA").map(std::path::PathBuf::from)
}

fn found_vaults() -> Vec<FoundVault> {
    app_data().map_or_else(Vec::new, |path| obsidian_source::found_vaults(&path))
}

pub fn status() -> Option<serde_json::Value> {
    handle().and_then(|h| serde_json::to_value(h.document()).ok())
}

pub fn found() -> Vec<FoundVault> {
    found_vaults()
}

fn to_value(document: ObsidianDocument) -> Option<serde_json::Value> {
    serde_json::to_value(document).ok()
}

/// Turn the source on for a vault Obsidian lists on this computer. The chosen
/// path must be one of those, so no path the owner typed or a page supplied can
/// reach the source this way.
pub async fn use_found_vault(path: String) -> Result<Option<serde_json::Value>, String> {
    let found = tauri::async_runtime::spawn_blocking(found_vaults)
        .await
        .map_err(|_| "obsidian_found_vaults_failed")?;
    let known = found.iter().any(|vault| vault.path == path);
    if !known {
        return Err("not_a_found_vault".into());
    }
    let Some(handle) = handle() else {
        return Ok(None);
    };
    Ok(to_value(handle.turn_on(path).await))
}

/// Ask the owner for a folder with the native dialog and turn the source on for
/// it. `Ok(None)` when the owner cancels the dialog.
pub async fn choose_folder(
    window: tauri::WebviewWindow,
) -> Result<Option<serde_json::Value>, String> {
    crate::logs::require_settings(&window)?;
    let owner = window.hwnd().map_err(|_| "obsidian_window_unavailable")?.0 as isize;
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        platform_win::folder_picker::choose_folder(owner, CHOOSER_TITLE)
    })
    .await
    .map_err(|_| "obsidian_dialog_failed")?
    .map_err(|_| "obsidian_dialog_failed")?;
    let Some(path) = chosen else {
        return Ok(None);
    };
    let Some(path) = path.to_str().map(str::to_owned) else {
        // A chosen folder whose path can't be represented for the engine.
        return Err("obsidian_unusable_path".into());
    };
    let Some(handle) = handle() else {
        return Ok(None);
    };
    Ok(to_value(handle.turn_on(path).await))
}

pub async fn turn_off() -> Option<serde_json::Value> {
    match handle() {
        Some(handle) => to_value(handle.turn_off().await),
        None => None,
    }
}

pub async fn continue_warning() -> Option<serde_json::Value> {
    match handle() {
        Some(handle) => to_value(handle.continue_warning().await),
        None => None,
    }
}

pub async fn cancel_warning() -> Option<serde_json::Value> {
    match handle() {
        Some(handle) => to_value(handle.cancel_warning().await),
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    use pl_transport_win::credential::{Credential, EndpointAddr};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        let cert_der = CertificateDer::from(cert.der().to_vec());
        let key_der = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
        (cert_der, key_der)
    }

    fn credential(pin: Vec<u8>, port: u16) -> Credential {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["observer.test".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        Credential {
            client_key_pem: key.serialize_pem(),
            client_cert_pem: cert.pem(),
            ca_chain_pem: vec![cert.pem()],
            ca_fp_prefix: pin,
            instance_id: "test-instance".into(),
            home_label: "Home".into(),
            endpoints: vec![EndpointAddr {
                host: "127.0.0.1".into(),
                port,
            }],
            relay_origin: None,
            device_token: None,
            device_token_expires_at: None,
        }
    }

    /// Answer each dial with the next `(status, body)` and keep what was sent.
    async fn start_journal(
        answers: Vec<(u16, String)>,
    ) -> (Arc<ObserverClient>, Arc<StdMutex<Vec<Vec<u8>>>>) {
        let (cert, key) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let requests = Arc::new(StdMutex::new(Vec::new()));
        let seen = Arc::clone(&requests);
        let config =
            ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
                .with_safe_default_protocol_versions()
                .unwrap()
                .with_no_client_auth()
                .with_single_cert(vec![cert], key)
                .unwrap();
        let acceptor = TlsAcceptor::from(Arc::new(config));
        tokio::spawn(async move {
            for (status, body) in answers {
                let (tcp, _) = listener.accept().await.unwrap();
                let mut tls = acceptor.accept(tcp).await.unwrap();
                let mut decoder = FrameDecoder::new();
                let mut request = Vec::new();
                let mut stream_id = 1u32;
                let mut closed = false;
                let mut buf = [0u8; 8192];
                while !closed {
                    let n = tls.read(&mut buf).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    decoder.feed(&buf[..n]);
                    for frame in decoder.drain().unwrap() {
                        stream_id = frame.stream_id;
                        if frame.flags & FLAG_DATA != 0 {
                            request.extend_from_slice(&frame.payload);
                        }
                        if frame.flags & FLAG_CLOSE != 0 {
                            closed = true;
                        }
                    }
                }
                seen.lock().unwrap().push(request);
                let reason = if status == 200 { "OK" } else { "Refused" };
                let response = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
                    body.len()
                );
                let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
                tls.write_all(&frame.encode().unwrap()).await.unwrap();
                tls.flush().await.unwrap();
                let _ = tls.shutdown().await;
            }
        });
        let client = Arc::new(
            ObserverClient::new(
                credential(pin, port),
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .unwrap(),
        );
        (client, requests)
    }

    fn journal_for(client: Arc<ObserverClient>) -> ClientJournal {
        journal_counting_answers(client).0
    }

    /// The journal seam, and how many times it has heard the journal answer.
    fn journal_counting_answers(
        client: Arc<ObserverClient>,
    ) -> (ClientJournal, Arc<std::sync::atomic::AtomicUsize>) {
        let load: LoadClient = Arc::new(move || {
            let client = Arc::clone(&client);
            Box::pin(async move { Some(client) })
        });
        let heard = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = Arc::clone(&heard);
        let journal = ClientJournal::new(
            load,
            Arc::new(|| {
                Some(LocalZone {
                    utc_offset_seconds: -7 * 3600,
                    tz: None,
                })
            }),
            Arc::new(move || {
                count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            }),
        );
        (journal, heard)
    }

    fn delivery_file(dir: &Path) -> Delivery {
        let name = format!("obsidian_{}.jsonl", "a".repeat(64));
        let path = dir.join(&name);
        std::fs::write(&path, b"{\"v\":1}\n").unwrap();
        Delivery {
            window: obsidian_source::Window {
                day: "20261108".into(),
                segment: "094000_300".into(),
            },
            name,
            path,
            lines: 1,
            first_sight: 1,
        }
    }

    const OK_BODY: &str = r#"{"status":"ok","segment":"094000_300","files":["note_transcript.md"],"bytes":60,"file_descriptors":[{"submitted":"note_transcript.md","written":"note_transcript.md","size":60,"sha256":"0000000000000000000000000000000000000000000000000000000000000000","disposition":"written"}],"meta":{}}"#;
    const DUPLICATE_BODY: &str = r#"{"status":"duplicate","existing_segment":"094000_300","message":"Nothing new in this file","file_descriptors":[],"meta":{}}"#;

    #[tokio::test]
    async fn the_answers_table_holds_over_the_real_upload_path() {
        let dir = tempfile::tempdir().unwrap();
        let delivery = delivery_file(dir.path());
        // Each pairing of the journal's literal bodies with what the app must do.
        let cases: Vec<(u16, String, Answer)> = vec![
            (200, OK_BODY.into(), Answer::Delivered),
            (200, DUPLICATE_BODY.into(), Answer::Delivered),
            (
                200,
                r#"{"status":"collision","segment":"094000_300","file_descriptors":[],"meta":{}}"#.into(),
                Answer::Delivered,
            ),
            (
                500,
                r#"{"status":"failed","error":"Ingest request failed","reason_code":"segment_removed","detail":"the owner removed this segment"}"#.into(),
                Answer::Delivered,
            ),
            (
                409,
                r#"{"error":"Ingest request failed","reason_code":"obsidian_note_ahead","detail":"this window is later than the journal's today; send it again later"}"#.into(),
                Answer::Ahead,
            ),
            (
                400,
                r#"{"error":"Ingest request failed","reason_code":"obsidian_note_invalid","detail":"bad"}"#.into(),
                Answer::Invalid,
            ),
            (
                500,
                r#"{"status":"failed","error":"Ingest request failed","reason_code":"journal_write_failed","detail":"x"}"#.into(),
                Answer::Retry,
            ),
            (503, "busy".into(), Answer::Retry),
        ];
        let answers = cases
            .iter()
            .map(|(status, body, _)| (*status, body.clone()))
            .collect();
        let (client, requests) = start_journal(answers).await;
        let journal = journal_for(client);
        for (index, (_, _, expected)) in cases.iter().enumerate() {
            assert_eq!(
                journal.deliver(&delivery).await,
                DeliveryResult::Answer(*expected),
                "case {index}: {:?}",
                cases[index].1
            );
        }
        // The envelope carries the source and the window; the part is named by its hash.
        let requests = requests.lock().unwrap();
        let body = String::from_utf8_lossy(&requests[0]);
        assert!(body.contains(r#""source":"obsidian""#), "{body}");
        assert!(body.contains(r#""day":"20261108""#), "{body}");
        assert!(body.contains(r#""segment":"094000_300""#), "{body}");
        assert!(
            body.contains(&format!("obsidian_{}.jsonl", "a".repeat(64))),
            "{body}"
        );
    }

    #[tokio::test]
    async fn a_delivery_the_journal_answers_counts_as_reaching_it() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempfile::tempdir().unwrap();
        let delivery = delivery_file(dir.path());

        let (client, _requests) = start_journal(vec![(200, OK_BODY.into())]).await;
        let (journal, heard) = journal_counting_answers(client);
        assert_eq!(
            journal.deliver(&delivery).await,
            DeliveryResult::Answer(Answer::Delivered)
        );
        assert_eq!(heard.load(Ordering::SeqCst), 1, "answered: reached");

        // Nothing listening: not reached.
        let (cert, _) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let unreachable = Arc::new(
            ObserverClient::new(credential(pin, 1), Arc::new(AtomicBool::new(true))).unwrap(),
        );
        let (journal, heard) = journal_counting_answers(unreachable);
        assert_eq!(
            journal.deliver(&delivery).await,
            DeliveryResult::Answer(Answer::Retry)
        );
        assert_eq!(heard.load(Ordering::SeqCst), 0, "no answer: not reached");
    }

    #[tokio::test]
    async fn capability_and_vault_list_read_the_journal() {
        let (client, _requests) = start_journal(vec![
            (
                200,
                r#"{"version":{"current":"x"},"capabilities":["calendar_notes","obsidian_notes"]}"#
                    .into(),
            ),
            (200, r#"{"version":{"current":"x"}}"#.into()),
            (
                200,
                r#"{"vaults":[{"vault":"Notes","device":"studio","last_synced_at":"2026-11-08T09:44:33-07:00"}]}"#
                    .into(),
            ),
            (404, "".into()),
            (200, "not json".into()),
        ])
        .await;
        let (journal, heard) = journal_counting_answers(client);
        assert_eq!(journal.capability().await, Capability::Supported);
        assert_eq!(journal.capability().await, Capability::NotListed);
        assert_eq!(
            heard.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "a capability answer is reaching the journal"
        );
        match journal.vault_list().await {
            VaultList::Listed(list) => {
                assert_eq!(list.len(), 1);
                assert_eq!(list[0].device.as_deref(), Some("studio"));
            }
            VaultList::Unavailable => panic!("expected a list"),
        }
        assert!(
            matches!(journal.vault_list().await, VaultList::Listed(list) if list.is_empty()),
            "a journal without the route has no vaults"
        );
        assert!(matches!(journal.vault_list().await, VaultList::Unavailable));
    }

    #[tokio::test]
    async fn an_unconfirmed_pairing_gets_no_answer_and_no_request() {
        let dir = tempfile::tempdir().unwrap();
        let delivery = delivery_file(dir.path());
        let (cert, _) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let client = Arc::new(
            ObserverClient::new(
                credential(pin, 1),
                Arc::new(std::sync::atomic::AtomicBool::new(false)),
            )
            .unwrap(),
        );
        assert_eq!(
            journal_for(client).deliver(&delivery).await,
            DeliveryResult::NoAnswer
        );
    }

    #[tokio::test]
    async fn a_file_gone_from_disk_is_its_own_outcome() {
        let dir = tempfile::tempdir().unwrap();
        let delivery = delivery_file(dir.path());
        std::fs::remove_file(&delivery.path).unwrap();
        let (cert, _) = self_signed();
        let pin = spl_core::ca::sha256(cert.as_ref())[..16].to_vec();
        let client = Arc::new(
            ObserverClient::new(
                credential(pin, 1),
                Arc::new(std::sync::atomic::AtomicBool::new(true)),
            )
            .unwrap(),
        );
        assert_eq!(
            journal_for(client).deliver(&delivery).await,
            DeliveryResult::FileMissing
        );
    }
}
