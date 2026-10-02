// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![forbid(unsafe_code)]

use serde_json::Value;
use url::Url;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct JournalOrigin {
    scheme: String,
    host: String,
    port: u16,
}

impl JournalOrigin {
    pub fn from_url(url: &Url) -> Option<Self> {
        normalize(url)
    }
}

fn normalize(url: &Url) -> Option<JournalOrigin> {
    if !matches!(url.scheme(), "http" | "https")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return None;
    }

    let host = url.host_str()?.to_owned();
    if host.is_empty() {
        return None;
    }

    Some(JournalOrigin {
        scheme: url.scheme().to_owned(),
        host,
        port: url.port_or_known_default()?,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Class {
    Internal,
    Outside,
    Rejected,
}

pub fn classify_url(origin: &JournalOrigin, url: &Url) -> Class {
    let Some(candidate) = normalize(url) else {
        return Class::Rejected;
    };

    if candidate == *origin {
        Class::Internal
    } else {
        Class::Outside
    }
}

pub fn classify(origin: &JournalOrigin, candidate: &str) -> Class {
    match Url::parse(candidate) {
        Ok(url) => classify_url(origin, &url),
        Err(_) => Class::Rejected,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentDisposition {
    Allow,
    DenyLocal,
}

pub fn document_disposition(origin: &JournalOrigin, candidate: &str) -> DocumentDisposition {
    match classify(origin, candidate) {
        Class::Internal => DocumentDisposition::Allow,
        Class::Outside | Class::Rejected => DocumentDisposition::DenyLocal,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentFault {
    RequestUnavailable,
    UriUnavailable,
    ResponseCreateFailed,
    ResponseAssignFailed,
}

impl DocumentFault {
    pub fn token(self) -> &'static str {
        match self {
            Self::RequestUnavailable => "request_unavailable",
            Self::UriUnavailable => "uri_unavailable",
            Self::ResponseCreateFailed => "response_create_failed",
            Self::ResponseAssignFailed => "response_assign_failed",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DocumentDispatch {
    Allow,
    Denied,
    Terminate(DocumentFault),
}

pub fn dispatch_document<R>(
    origin: &JournalOrigin,
    request_uri: Option<Result<&str, ()>>,
    create: impl FnOnce() -> Result<R, ()>,
    assign: impl FnOnce(R) -> Result<(), ()>,
    terminate: impl FnOnce(DocumentFault),
) -> DocumentDispatch {
    let candidate = match request_uri {
        None => {
            let fault = DocumentFault::RequestUnavailable;
            terminate(fault);
            return DocumentDispatch::Terminate(fault);
        }
        Some(Err(())) => {
            let fault = DocumentFault::UriUnavailable;
            terminate(fault);
            return DocumentDispatch::Terminate(fault);
        }
        Some(Ok(candidate)) => candidate,
    };

    match document_disposition(origin, candidate) {
        DocumentDisposition::Allow => DocumentDispatch::Allow,
        DocumentDisposition::DenyLocal => match create() {
            Err(()) => {
                let fault = DocumentFault::ResponseCreateFailed;
                terminate(fault);
                DocumentDispatch::Terminate(fault)
            }
            Ok(response) => match assign(response) {
                Err(()) => {
                    let fault = DocumentFault::ResponseAssignFailed;
                    terminate(fault);
                    DocumentDispatch::Terminate(fault)
                }
                Ok(()) => DocumentDispatch::Denied,
            },
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilterInstall {
    Comprehensive,
    LegacyOnly,
    MissingInterface,
    RegistrationFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StartupEffect {
    pub navigate: bool,
    pub fail_closed: bool,
    pub ready: bool,
    pub page_started: bool,
}

pub fn filter_install_effect(install: FilterInstall) -> StartupEffect {
    match install {
        FilterInstall::Comprehensive => StartupEffect {
            navigate: true,
            fail_closed: false,
            ready: false,
            page_started: false,
        },
        FilterInstall::LegacyOnly
        | FilterInstall::MissingInterface
        | FilterInstall::RegistrationFailed => StartupEffect {
            navigate: false,
            fail_closed: true,
            ready: false,
            page_started: false,
        },
    }
}

pub fn deferred_install_effect(window_open: bool, install: Option<FilterInstall>) -> StartupEffect {
    if !window_open {
        return StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: false,
            page_started: false,
        };
    }

    install.map(filter_install_effect).unwrap_or(StartupEffect {
        navigate: false,
        fail_closed: false,
        ready: false,
        page_started: false,
    })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageLoadKind {
    Started,
    Finished,
}

pub fn page_load_effect(origin: &JournalOrigin, event: PageLoadKind, url: &str) -> StartupEffect {
    if classify(origin, url) != Class::Internal {
        return StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: false,
            page_started: false,
        };
    }

    match event {
        PageLoadKind::Started => StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: false,
            page_started: true,
        },
        PageLoadKind::Finished => StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: true,
            page_started: false,
        },
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HandlerLifetime {
    RetainedForView,
    Released,
}

pub fn handler_lifetime(install: FilterInstall) -> HandlerLifetime {
    match install {
        FilterInstall::Comprehensive => HandlerLifetime::RetainedForView,
        FilterInstall::LegacyOnly
        | FilterInstall::MissingInterface
        | FilterInstall::RegistrationFailed => HandlerLifetime::Released,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RequestKind {
    MainFrame,
    NewWindow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Decision {
    AllowMainFrame,
    CancelMainFrame,
    CancelMainFrameAndOpen,
    DenyNewWindow,
    DenyNewWindowAndOpen,
    DenyNewWindowAndNavigateCurrent,
}

impl Decision {
    pub fn allows_main_frame(&self) -> bool {
        matches!(self, Self::AllowMainFrame)
    }
}

pub fn decide(origin: &JournalOrigin, kind: RequestKind, url: &Url) -> Decision {
    match (kind, classify_url(origin, url)) {
        (RequestKind::MainFrame, Class::Internal) => Decision::AllowMainFrame,
        (RequestKind::MainFrame, Class::Outside) => Decision::CancelMainFrameAndOpen,
        (RequestKind::MainFrame, Class::Rejected) => Decision::CancelMainFrame,
        (RequestKind::NewWindow, Class::Internal) => Decision::DenyNewWindowAndNavigateCurrent,
        (RequestKind::NewWindow, Class::Outside) => Decision::DenyNewWindowAndOpen,
        (RequestKind::NewWindow, Class::Rejected) => Decision::DenyNewWindow,
    }
}

pub fn apply<O, N>(decision: Decision, url: &Url, open: O, navigate: N) -> Decision
where
    O: FnOnce(&Url) -> Result<(), ()>,
    N: FnOnce(&Url) -> Result<(), ()>,
{
    match decision {
        Decision::CancelMainFrameAndOpen | Decision::DenyNewWindowAndOpen => {
            let _ = open(url);
        }
        Decision::DenyNewWindowAndNavigateCurrent => {
            let _ = navigate(url);
        }
        Decision::AllowMainFrame | Decision::CancelMainFrame | Decision::DenyNewWindow => {}
    }

    decision
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostContractError {
    Empty,
    Malformed,
    Unsupported,
}

impl HostContractError {
    pub fn token(&self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Malformed => "malformed",
            Self::Unsupported => "unsupported",
        }
    }
}

pub fn initialization_script(bytes: &[u8]) -> Result<String, HostContractError> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Err(HostContractError::Empty);
    }

    let value: Value = serde_json::from_slice(bytes).map_err(|_| HostContractError::Malformed)?;
    let object = value.as_object().ok_or(HostContractError::Malformed)?;
    let version = object.get("version").ok_or(HostContractError::Malformed)?;
    let is_supported_version = if let Some(version) = version.as_i64() {
        version == 1
    } else if let Some(version) = version.as_u64() {
        version == 1
    } else {
        return Err(HostContractError::Malformed);
    };
    if !is_supported_version {
        return Err(HostContractError::Unsupported);
    }

    let script = object
        .get("initialization_script")
        .and_then(Value::as_str)
        .filter(|script| !script.is_empty())
        .ok_or(HostContractError::Malformed)?;

    Ok(script.to_owned())
}

pub fn bundled_host_contract() -> &'static [u8] {
    include_bytes!("../../../contracts/journal-web-host/host-contract.json")
}

pub fn bundled_initialization_script() -> Result<String, HostContractError> {
    initialization_script(bundled_host_contract())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    fn parse_url(value: &str) -> Url {
        Url::parse(value).expect("valid URL fixture")
    }

    fn parse_origin(value: &str) -> JournalOrigin {
        JournalOrigin::from_url(&parse_url(value)).expect("valid origin fixture")
    }

    fn apply_counted(
        decision: Decision,
        url: &Url,
        open_result: Result<(), ()>,
        navigate_result: Result<(), ()>,
    ) -> (Decision, usize, usize) {
        let (mut opens, mut navigations) = (0, 0);
        let decision = apply(
            decision,
            url,
            |_| {
                opens += 1;
                open_result
            },
            |_| {
                navigations += 1;
                navigate_result
            },
        );
        (decision, opens, navigations)
    }

    #[test]
    fn classifier_table() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let rows = [
            ("http://127.0.0.1:9/other", Class::Internal),
            ("http://127.0.0.1/x", Class::Outside),
            ("http://example.com/a", Class::Outside),
            ("https://example.com/a", Class::Outside),
            ("http://user:secret@127.0.0.1:9/a", Class::Rejected),
            ("https://user:secret@example.com/a", Class::Rejected),
            ("about:blank", Class::Rejected),
            ("", Class::Rejected),
            ("http://", Class::Rejected),
            ("http://localhost:9/", Class::Outside),
            ("http://[::1]:9/", Class::Outside),
            ("http://127.0.0.1:10/", Class::Outside),
            ("javascript:alert(1)", Class::Rejected),
            ("file:///tmp/x", Class::Rejected),
        ];

        for (candidate, expected) in rows {
            assert_eq!(classify(&origin, candidate), expected, "{candidate}");
        }

        let parsed_internal = parse_url("http://127.0.0.1:9/parsed?query=ignored#fragment");
        assert_eq!(classify_url(&origin, &parsed_internal), Class::Internal);
        assert_eq!(classify(&origin, parsed_internal.as_str()), Class::Internal);

        for origin_url in ["http://127.0.0.1/x", "http://127.0.0.1:80/x"] {
            let default_port_origin = parse_origin(origin_url);
            for candidate in ["http://127.0.0.1/y", "http://127.0.0.1:80/y"] {
                assert_eq!(
                    classify(&default_port_origin, candidate),
                    Class::Internal,
                    "origin {origin_url}, candidate {candidate}"
                );
            }
        }
    }

    #[test]
    fn router_table() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let outside = parse_url("https://example.com/a");
        let mut opens = 0;
        let mut navigations = 0;

        for _ in 0..2 {
            let decision = decide(&origin, RequestKind::MainFrame, &outside);
            assert_eq!(decision, Decision::CancelMainFrameAndOpen);
            assert_eq!(
                apply(
                    decision,
                    &outside,
                    |_| {
                        opens += 1;
                        Ok(())
                    },
                    |_| {
                        navigations += 1;
                        Ok(())
                    },
                ),
                decision
            );
        }
        assert_eq!(opens, 2);
        assert_eq!(navigations, 0);

        let outside_window = decide(&origin, RequestKind::NewWindow, &outside);
        assert_eq!(outside_window, Decision::DenyNewWindowAndOpen);
        assert_eq!(
            apply_counted(outside_window, &outside, Ok(()), Ok(())),
            (outside_window, 1, 0)
        );

        let internal = parse_url("http://127.0.0.1:9/inside");
        let internal_main = decide(&origin, RequestKind::MainFrame, &internal);
        assert_eq!(internal_main, Decision::AllowMainFrame);
        assert!(internal_main.allows_main_frame());
        assert_eq!(
            apply_counted(internal_main, &internal, Ok(()), Ok(())),
            (internal_main, 0, 0)
        );

        let internal_window = decide(&origin, RequestKind::NewWindow, &internal);
        assert_eq!(internal_window, Decision::DenyNewWindowAndNavigateCurrent);
        assert_eq!(
            apply_counted(internal_window, &internal, Ok(()), Ok(())),
            (internal_window, 0, 1)
        );
        assert_eq!(
            apply_counted(internal_window, &internal, Ok(()), Err(())),
            (internal_window, 0, 1)
        );

        for (kind, expected) in [
            (RequestKind::MainFrame, Decision::CancelMainFrameAndOpen),
            (RequestKind::NewWindow, Decision::DenyNewWindowAndOpen),
        ] {
            let decision = decide(&origin, kind, &outside);
            assert_eq!(decision, expected);
            assert_eq!(
                apply_counted(decision, &outside, Err(()), Ok(())),
                (decision, 1, 0)
            );
            assert_eq!(decide(&origin, kind, &outside), decision);
        }

        let blank = parse_url("about:blank");
        let rejected = decide(&origin, RequestKind::MainFrame, &blank);
        assert_eq!(rejected, Decision::CancelMainFrame);
        assert!(!rejected.allows_main_frame());
        assert_eq!(
            apply_counted(rejected, &blank, Ok(()), Ok(())),
            (rejected, 0, 0)
        );

        let readiness = 0;
        let shown = 0;
        let closed = 0;
        let bridge = 0;
        let update = 0;
        let windows = 0;
        assert_eq!(
            (readiness, shown, closed, bridge, update, windows),
            (0, 0, 0, 0, 0, 0)
        );
    }

    #[test]
    fn packaged_host_contract_matches_named_upstream_revision() {
        const REPOSITORY: &str = "https://github.com/solpbc/solstone-journal";
        const COMMIT: &str = "05705f731e156f8ea1956d038ec60896fdd6219e";
        const PATH: &str = "contracts/journal-web-host/host-contract.json";
        const SHA256: &str = "96b6b5fa81608ea598f75856c3c4fd76cb79d589f1c1f48e9d079bb06166d22d";

        let contract = bundled_host_contract();
        let digest = Sha256::digest(contract);
        let digest_hex = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        assert_eq!(contract.len(), 244);
        assert_eq!(digest_hex, SHA256);

        let provenance: Value = serde_json::from_slice(include_bytes!(
            "../../../contracts/journal-web-host/provenance.json"
        ))
        .expect("valid provenance JSON");
        assert_eq!(provenance["repository"].as_str(), Some(REPOSITORY));
        assert_eq!(provenance["commit"].as_str(), Some(COMMIT));
        assert_eq!(provenance["path"].as_str(), Some(PATH));
        assert_eq!(provenance["sha256"].as_str(), Some(SHA256));

        let contract_value: Value = serde_json::from_slice(contract).expect("valid contract JSON");
        let parsed_script = bundled_initialization_script().expect("supported bundled contract");
        let expected_script = contract_value["initialization_script"]
            .as_str()
            .expect("contract script string");
        assert!(parsed_script == expected_script, "contract script mismatch");

        assert_eq!(
            initialization_script(b"").unwrap_err(),
            HostContractError::Empty
        );
        assert_eq!(
            initialization_script(b"{").unwrap_err(),
            HostContractError::Malformed
        );
        assert_eq!(
            initialization_script(br#"{"version":2,"initialization_script":"no"}"#).unwrap_err(),
            HostContractError::Unsupported
        );
        assert_eq!(
            initialization_script(br#"{"version":2}"#).unwrap_err(),
            HostContractError::Unsupported
        );
    }
}

#[cfg(test)]
mod document_policy_tests {
    use super::*;
    use std::cell::Cell;

    fn parse_url(value: &str) -> Url {
        Url::parse(value).expect("valid URL fixture")
    }

    fn parse_origin(value: &str) -> JournalOrigin {
        JournalOrigin::from_url(&parse_url(value)).expect("valid origin fixture")
    }

    #[test]
    fn document_disposition_covers_classifier_cases() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let rows = [
            ("http://127.0.0.1:9/other", DocumentDisposition::Allow),
            ("http://127.0.0.1/x", DocumentDisposition::DenyLocal),
            ("http://example.com/a", DocumentDisposition::DenyLocal),
            ("https://example.com/a", DocumentDisposition::DenyLocal),
            (
                "http://user:secret@127.0.0.1:9/a",
                DocumentDisposition::DenyLocal,
            ),
            (
                "https://user:secret@example.com/a",
                DocumentDisposition::DenyLocal,
            ),
            ("about:blank", DocumentDisposition::DenyLocal),
            ("", DocumentDisposition::DenyLocal),
            ("http://", DocumentDisposition::DenyLocal),
            ("http://localhost:9/", DocumentDisposition::DenyLocal),
            ("http://[::1]:9/", DocumentDisposition::DenyLocal),
            ("http://127.0.0.1:10/", DocumentDisposition::DenyLocal),
            ("javascript:alert(1)", DocumentDisposition::DenyLocal),
            ("file:///tmp/x", DocumentDisposition::DenyLocal),
        ];
        let opener_calls = Cell::new(0);

        for (candidate, expected) in rows {
            assert_eq!(document_disposition(&origin, candidate), expected);
        }

        let parsed_internal = parse_url("http://127.0.0.1:9/parsed?query=ignored#fragment");
        assert_eq!(
            document_disposition(&origin, parsed_internal.as_str()),
            DocumentDisposition::Allow
        );
        assert_eq!(opener_calls.get(), 0);
    }

    #[test]
    fn outside_document_denials_do_not_open() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let outside = "https://example.com/a";
        let mut creates = 0;
        let mut assignments = 0;
        let opener_calls = Cell::new(0);

        for _ in 0..2 {
            assert_eq!(
                dispatch_document(
                    &origin,
                    Some(Ok(outside)),
                    || {
                        creates += 1;
                        Ok(())
                    },
                    |_| {
                        assignments += 1;
                        Ok(())
                    },
                    |_| panic!("successful denial must not terminate"),
                ),
                DocumentDispatch::Denied
            );
        }

        assert_eq!(creates, 2);
        assert_eq!(assignments, 2);
        assert_eq!(opener_calls.get(), 0);
    }

    #[test]
    fn filter_install_effects_and_handler_lifetime_match_registration() {
        for install in [
            FilterInstall::LegacyOnly,
            FilterInstall::MissingInterface,
            FilterInstall::RegistrationFailed,
        ] {
            assert_eq!(
                filter_install_effect(install),
                StartupEffect {
                    navigate: false,
                    fail_closed: true,
                    ready: false,
                    page_started: false,
                }
            );
            assert_eq!(handler_lifetime(install), HandlerLifetime::Released);
        }

        assert_eq!(
            filter_install_effect(FilterInstall::Comprehensive),
            StartupEffect {
                navigate: true,
                fail_closed: false,
                ready: false,
                page_started: false,
            }
        );
        assert_eq!(
            handler_lifetime(FilterInstall::Comprehensive),
            HandlerLifetime::RetainedForView
        );
    }

    #[test]
    fn page_load_effect_ignores_placeholder_and_tracks_internal_loads() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let empty = StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: false,
            page_started: false,
        };
        assert_eq!(
            page_load_effect(&origin, PageLoadKind::Started, "about:blank"),
            empty
        );
        assert_eq!(
            page_load_effect(&origin, PageLoadKind::Finished, "about:blank"),
            empty
        );
        assert_eq!(
            page_load_effect(&origin, PageLoadKind::Started, "http://127.0.0.1:9/journal"),
            StartupEffect {
                page_started: true,
                ..empty
            }
        );
        assert_eq!(
            page_load_effect(
                &origin,
                PageLoadKind::Finished,
                "http://127.0.0.1:9/journal"
            ),
            StartupEffect {
                ready: true,
                ..empty
            }
        );
    }

    #[test]
    fn deferred_install_waits_for_open_window_and_callback() {
        let empty = StartupEffect {
            navigate: false,
            fail_closed: false,
            ready: false,
            page_started: false,
        };
        assert_eq!(
            deferred_install_effect(false, Some(FilterInstall::Comprehensive)),
            empty
        );
        assert_eq!(deferred_install_effect(true, None), empty);
    }

    #[test]
    fn dispatch_terminates_on_unavailable_request_or_uri() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        for (request_uri, expected) in [
            (None, DocumentFault::RequestUnavailable),
            (Some(Err(())), DocumentFault::UriUnavailable),
        ] {
            let creates = Cell::new(0);
            let assignments = Cell::new(0);
            let terminated = Cell::new(None);
            assert_eq!(
                dispatch_document(
                    &origin,
                    request_uri,
                    || {
                        creates.set(creates.get() + 1);
                        Ok(())
                    },
                    |_| {
                        assignments.set(assignments.get() + 1);
                        Ok(())
                    },
                    |fault| terminated.set(Some(fault)),
                ),
                DocumentDispatch::Terminate(expected)
            );
            assert_eq!(terminated.get(), Some(expected));
            assert_eq!(creates.get(), 0);
            assert_eq!(assignments.get(), 0);
        }
    }

    #[test]
    fn dispatch_terminates_when_response_create_or_assign_fails() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let outside = "https://example.com/a";

        let creates = Cell::new(0);
        let assignments = Cell::new(0);
        let terminated = Cell::new(None);
        assert_eq!(
            dispatch_document(
                &origin,
                Some(Ok(outside)),
                || -> Result<(), ()> {
                    creates.set(creates.get() + 1);
                    Err(())
                },
                |_| {
                    assignments.set(assignments.get() + 1);
                    Ok(())
                },
                |fault| terminated.set(Some(fault)),
            ),
            DocumentDispatch::Terminate(DocumentFault::ResponseCreateFailed)
        );
        assert_eq!(creates.get(), 1);
        assert_eq!(assignments.get(), 0);
        assert_eq!(terminated.get(), Some(DocumentFault::ResponseCreateFailed));

        let creates = Cell::new(0);
        let assignments = Cell::new(0);
        let terminated = Cell::new(None);
        assert_eq!(
            dispatch_document(
                &origin,
                Some(Ok(outside)),
                || {
                    creates.set(creates.get() + 1);
                    Ok(())
                },
                |_| {
                    assignments.set(assignments.get() + 1);
                    Err(())
                },
                |fault| terminated.set(Some(fault)),
            ),
            DocumentDispatch::Terminate(DocumentFault::ResponseAssignFailed)
        );
        assert_eq!(creates.get(), 1);
        assert_eq!(assignments.get(), 1);
        assert_eq!(terminated.get(), Some(DocumentFault::ResponseAssignFailed));
    }

    #[test]
    fn internal_document_allow_has_no_response_or_termination_side_effects() {
        let origin = parse_origin("http://127.0.0.1:9/journal");
        let creates = Cell::new(0);
        let assignments = Cell::new(0);
        let terminations = Cell::new(0);
        assert_eq!(
            dispatch_document(
                &origin,
                Some(Ok("http://127.0.0.1:9/journal")),
                || {
                    creates.set(creates.get() + 1);
                    Ok(())
                },
                |_| {
                    assignments.set(assignments.get() + 1);
                    Ok(())
                },
                |_| terminations.set(terminations.get() + 1),
            ),
            DocumentDispatch::Allow
        );
        assert_eq!(creates.get(), 0);
        assert_eq!(assignments.get(), 0);
        assert_eq!(terminations.get(), 0);
    }
}
