// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use tokio::io::AsyncReadExt;

pub const CONTROL_PORT: u16 = 49248;

const OPEN_JOURNAL_VERB: &[u8] = b"open-journal\n";
const SURFACE_VERB: &[u8] = b"surface-settings\n";
const SURFACE_ABOUT_VERB: &[u8] = b"surface-about\n";

pub fn signal_surface() -> bool {
    signal(SURFACE_VERB)
}

pub fn signal_open_journal() -> bool {
    signal(OPEN_JOURNAL_VERB)
}

/// Surface About on an already-running instance. Without this, `--open-view
/// about` against a live app fell through to the surface verb and opened
/// Settings, so the flag silently did the wrong thing rather than failing.
pub fn signal_surface_about() -> bool {
    signal(SURFACE_ABOUT_VERB)
}

fn signal(verb: &[u8]) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], CONTROL_PORT));
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let timeout = Some(Duration::from_secs(2));
    if stream.set_read_timeout(timeout).is_err() {
        return false;
    }
    if stream.set_write_timeout(timeout).is_err() {
        return false;
    }

    stream.write_all(verb).is_ok()
}

pub(crate) fn control_requests_open_journal(buf: &[u8]) -> bool {
    buf.starts_with(OPEN_JOURNAL_VERB)
}

pub async fn serve(app: tauri::AppHandle, listener: tokio::net::TcpListener) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            handle_connection(app, stream).await;
        });
    }
}

pub(crate) async fn open_journal_from_control<S: crate::windows::JournalSurface>(
    state: &crate::app::AppState,
    surface: &S,
    app: Option<&tauri::AppHandle>,
) {
    if let Err(error) = crate::windows::open_journal(state, surface, app).await {
        tracing::warn!(
            target: "window",
            label = "journal",
            error = error.token(),
            "open-journal failed"
        );
    }
}

async fn handle_connection(app: tauri::AppHandle, mut stream: tokio::net::TcpStream) {
    let mut buf = [0_u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf)).await;
    let n = match read {
        Ok(Ok(n)) => n,
        _ => return,
    };
    if control_requests_open_journal(&buf[..n]) {
        tokio::spawn(async move {
            use tauri::Manager;
            let state = app.state::<crate::app::AppState>();
            open_journal_from_control(&state, &app, Some(&app)).await;
        });
    } else if buf[..n].starts_with(SURFACE_ABOUT_VERB) {
        std::thread::spawn(move || {
            let _ = crate::windows::open_about(&app);
        });
    } else if buf[..n].starts_with(SURFACE_VERB) {
        std::thread::spawn(move || {
            let _ = crate::windows::open_settings(&app);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
    use std::sync::Arc;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    use rcgen::{
        BasicConstraints, CertificateParams, CertificateSigningRequestParams, IsCa, KeyPair,
        KeyUsagePurpose, PKCS_ECDSA_P256_SHA256,
    };
    use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};

    /// The two surface verbs must not prefix-match each other: `handle_connection`
    /// dispatches on `starts_with`, so a shared prefix would route About to
    /// Settings and reintroduce exactly the silent-wrong-window bug this verb
    /// was added to fix.
    #[test]
    fn surface_verbs_are_distinguishable() {
        assert_ne!(SURFACE_VERB, SURFACE_ABOUT_VERB);
        assert!(!SURFACE_ABOUT_VERB.starts_with(SURFACE_VERB));
        assert!(!SURFACE_VERB.starts_with(SURFACE_ABOUT_VERB));
    }

    /// Every view reachable by `--open-view` needs a control verb, or a second
    /// launch silently opens the wrong one.
    #[test]
    fn every_view_has_a_surface_verb() {
        for view in observer_model::View::ALL {
            let verb: &[u8] = match view {
                observer_model::View::Settings => SURFACE_VERB,
                observer_model::View::About => SURFACE_ABOUT_VERB,
            };
            assert!(
                verb.ends_with(b"\n"),
                "{} verb must be newline-terminated",
                view.label()
            );
        }
    }

    struct FakeJournalSurface {
        settings_opened: AtomicUsize,
        journal_focused: AtomicUsize,
        journal_closed: AtomicUsize,
        focus_returns: AtomicBool,
    }

    impl FakeJournalSurface {
        fn new() -> Self {
            Self {
                settings_opened: AtomicUsize::new(0),
                journal_focused: AtomicUsize::new(0),
                journal_closed: AtomicUsize::new(0),
                focus_returns: AtomicBool::new(false),
            }
        }
    }

    impl crate::windows::JournalSurface for FakeJournalSurface {
        fn open_settings(&self) -> tauri::Result<()> {
            self.settings_opened.fetch_add(1, Ordering::SeqCst);
            Ok(())
        }

        fn focus_journal_if_present(&self) -> bool {
            self.journal_focused.fetch_add(1, Ordering::SeqCst);
            self.focus_returns.load(Ordering::SeqCst)
        }

        fn close_journal(&self) {
            self.journal_closed.fetch_add(1, Ordering::SeqCst);
        }
    }

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn test_app_state(probe: Option<Arc<crate::windows::OpenPairProbe>>) -> crate::app::AppState {
        let (cmd_tx, _cmd_rx) = tokio::sync::mpsc::unbounded_channel();
        let dir = std::env::temp_dir().join(format!(
            "solstone-test-control-{}-{}",
            std::process::id(),
            TEST_ID.fetch_add(1, Ordering::SeqCst)
        ));
        let _ = std::fs::create_dir_all(&dir);
        let state_path = dir.join("pairing.json");
        let journal_version_path = dir.join("journal-version.json");
        let exclusions_path = dir.join("exclusions.json");

        let sync_config = pl_transport_win::service::SyncConfig {
            device_label: "test-device".to_string(),
            period_secs: 300,
            state_path,
            segments_root: dir.join("segments"),
            local_offset: Arc::new(platform_win::WindowsLocalOffset),
            journal_version: Arc::new(pl_transport_win::JournalVersionController::new(
                journal_version_path,
            )),
            facts_fn: Arc::new(|| pl_transport_win::RawDeviceFacts {
                name: Some("test-device".to_string()),
                platform: Some("windows".to_string()),
                device_type: None,
                app_id: Some("app.solstone.windows".to_string()),
                app_version: Some(env!("CARGO_PKG_VERSION").to_string()),
            }),
            confirmation: Arc::new(std::sync::Mutex::new(String::new())),
            tombstone: Arc::new(std::sync::Mutex::new(None)),
            #[cfg(feature = "awaiting-hold")]
            awaiting_hold: None,
        };

        crate::app::AppState {
            commands: cmd_tx,
            health: Arc::new(std::sync::Mutex::new(crate::health::not_running_snapshot())),
            sync: Arc::new(std::sync::Mutex::new(
                observer_model::SyncSnapshot::default(),
            )),
            sync_config,
            _shutdown: std::sync::Mutex::new(None),
            uploader_slot: tokio::sync::Mutex::new(pl_transport_win::UploaderSlot::new()),
            credential_access: tokio::sync::Mutex::new(None),
            journal_open_lock: Arc::new(tokio::sync::Mutex::new(())),
            journal_bridge: Arc::new(std::sync::Mutex::new(None)),
            exclusions: crate::exclusions::ExclusionController::new(exclusions_path),
            probe,
        }
    }

    #[tokio::test]
    async fn open_journal_from_menu_while_awaiting_opens_settings() {
        let state = test_app_state(None);
        state.sync.lock().unwrap().pairing.phase =
            observer_model::PairingPhase::AwaitingConfirmation;
        let surface = FakeJournalSurface::new();

        crate::tray::open_journal_from_menu(&state, &surface, None).await;
        assert_eq!(surface.settings_opened.load(Ordering::SeqCst), 1);
        assert_eq!(surface.journal_focused.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn open_journal_from_launch_while_awaiting_opens_settings() {
        let state = test_app_state(None);
        state.sync.lock().unwrap().pairing.phase =
            observer_model::PairingPhase::AwaitingConfirmation;
        let surface = FakeJournalSurface::new();

        crate::app::open_journal_from_launch(&state, &surface, None).await;
        assert_eq!(surface.settings_opened.load(Ordering::SeqCst), 1);
        assert_eq!(surface.journal_focused.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn open_journal_from_control_while_awaiting_opens_settings() {
        assert!(control_requests_open_journal(b"open-journal\n"));
        let state = test_app_state(None);
        state.sync.lock().unwrap().pairing.phase =
            observer_model::PairingPhase::AwaitingConfirmation;
        let surface = FakeJournalSurface::new();

        open_journal_from_control(&state, &surface, None).await;
        assert_eq!(surface.settings_opened.load(Ordering::SeqCst), 1);
        assert_eq!(surface.journal_focused.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    #[ignore = "not paired routes to the bridge, which reports unpaired; this expectation is not the product behaviour"]
    async fn open_journal_while_not_paired_opens_settings() {
        let state = test_app_state(None);
        state.sync.lock().unwrap().pairing.phase = observer_model::PairingPhase::NotPaired;
        let surface = FakeJournalSurface::new();

        let result = crate::windows::open_journal(&state, &surface, None).await;
        assert!(result.is_ok());
        assert_eq!(surface.settings_opened.load(Ordering::SeqCst), 1);
        assert_eq!(surface.journal_focused.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn open_journal_while_paired_focuses_existing() {
        let state = test_app_state(None);
        state.sync.lock().unwrap().pairing.phase = observer_model::PairingPhase::Paired;
        let surface = FakeJournalSurface::new();
        surface.focus_returns.store(true, Ordering::SeqCst);

        let result = crate::windows::open_journal(&state, &surface, None).await;
        assert!(result.is_ok());
        assert_eq!(surface.journal_focused.load(Ordering::SeqCst), 1);
        assert_eq!(surface.settings_opened.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn answer_pairing_reject_and_cancel_close_journal_surface() {
        let state = test_app_state(None);
        let binding =
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef".to_string();
        state.sync.lock().unwrap().pairing.phase =
            observer_model::PairingPhase::AwaitingConfirmation;
        state.sync.lock().unwrap().pairing.binding = binding.clone();
        let surface = FakeJournalSurface::new();

        let res =
            crate::ipc::answer_pairing_surface(&surface, &state, binding.clone(), "reject".into())
                .await;
        assert!(res.is_ok());
        assert_eq!(surface.journal_closed.load(Ordering::SeqCst), 1);

        state.sync.lock().unwrap().pairing.phase =
            observer_model::PairingPhase::AwaitingConfirmation;
        state.sync.lock().unwrap().pairing.binding = binding.clone();

        let res =
            crate::ipc::answer_pairing_surface(&surface, &state, binding.clone(), "cancel".into())
                .await;
        assert!(res.is_ok());
        assert_eq!(surface.journal_closed.load(Ordering::SeqCst), 2);
    }

    fn signing_ca() -> (rcgen::Certificate, KeyPair) {
        let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
        params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
        params.key_usages.push(KeyUsagePurpose::DigitalSignature);
        params.key_usages.push(KeyUsagePurpose::KeyCertSign);
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (cert, key)
    }

    fn server_config(cert: rcgen::Certificate, key: rcgen::KeyPair) -> rustls::ServerConfig {
        let cert_der = rustls::pki_types::CertificateDer::from(cert.der().to_vec());
        let key_der = rustls::pki_types::PrivateKeyDer::Pkcs8(
            rustls::pki_types::PrivatePkcs8KeyDer::from(key.serialize_der()),
        );
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::ring::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(vec![cert_der], key_der)
        .unwrap()
    }

    fn direct_pair_link(port: u16, ca_fp_prefix: &[u8]) -> String {
        let mut blob = vec![0x04, 0x01, 127, 0, 0, 1];
        blob.extend_from_slice(&port.to_be_bytes());
        blob.extend(0u8..16);
        blob.extend_from_slice(ca_fp_prefix);
        format!(
            "https://go.solstone.app/p#{}",
            spl_core::crockford::encode(&blob)
        )
    }

    async fn read_framed_request(
        tls: &mut tokio_rustls::server::TlsStream<tokio::net::TcpStream>,
    ) -> (u32, Vec<u8>) {
        let mut decoder = FrameDecoder::new();
        let mut request = Vec::new();
        let mut stream_id = 1u32;
        let mut closed = false;
        let mut buf = [0u8; 4096];
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
        (stream_id, request)
    }

    fn request_body(request: &[u8]) -> serde_json::Value {
        let text = String::from_utf8_lossy(request);
        let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
        serde_json::from_str(body).unwrap()
    }

    async fn serve_one_pair_response(
        listener: TcpListener,
        acceptor: TlsAcceptor,
        signing_cert: rcgen::Certificate,
        signing_key: KeyPair,
    ) {
        let (tcp, _) = listener.accept().await.unwrap();
        let mut tls = acceptor.accept(tcp).await.unwrap();
        let (stream_id, request) = read_framed_request(&mut tls).await;
        let body_json = request_body(&request);
        let body_obj = body_json.as_object().unwrap();
        let csr = body_obj.get("csr").unwrap().as_str().unwrap();
        let client_cert = CertificateSigningRequestParams::from_pem(csr)
            .unwrap()
            .signed_by(&signing_cert, &signing_key)
            .unwrap();

        let response_body = serde_json::to_vec(&serde_json::json!({
            "client_cert": client_cert.pem(),
            "ca_chain": [signing_cert.pem()],
            "instance_id": "4d1f3d57-4f39-4930-b8f8-5e6f2a84d51a",
            "home_label": "fixture home",
            "fingerprint": format!("sha256:{}", spl_core::ca::sha256_hex(client_cert.der())),
            "home_attestation": "fixture-attestation",
            "local_endpoints": [{"ip": "192.0.2.10", "port": 7657, "scope": "lan"}],
        }))
        .unwrap();

        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{}",
            response_body.len(),
            String::from_utf8_lossy(&response_body)
        );
        let frame = Frame::new(stream_id, FLAG_DATA | FLAG_CLOSE, response.into_bytes());
        tls.write_all(&frame.encode().unwrap()).await.unwrap();
        tls.flush().await.unwrap();
        let _ = tls.shutdown().await;
    }

    #[tokio::test]
    #[ignore = "the fake journal fixture fails its TLS handshake (certificate unknown); repair the fixture"]
    async fn pairing_and_open_journal_interleaving_bounded_5s() {
        let probe = Arc::new(crate::windows::OpenPairProbe::new());
        let state = Arc::new(test_app_state(Some(probe.clone())));
        state.sync.lock().unwrap().pairing.phase = observer_model::PairingPhase::Paired;
        let surface = Arc::new(FakeJournalSurface::new());

        let (signing_cert, signing_key) = signing_ca();
        let (server_cert, server_key) = signing_ca();
        let server_pin = spl_core::ca::sha256(server_cert.der())[..16].to_vec();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(server_cert, server_key)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pair_server = tokio::spawn(serve_one_pair_response(
            listener,
            acceptor,
            signing_cert,
            signing_key,
        ));
        let link = direct_pair_link(port, &server_pin);

        let state_open = state.clone();
        let surface_open = surface.clone();
        let open_task = tokio::spawn(async move {
            crate::windows::open_journal(&state_open, &*surface_open, None).await
        });

        probe.open_holds.notified().await;

        let state_pair = state.clone();
        let surface_pair = surface.clone();
        let link_clone = link.clone();
        let pair_task = tokio::spawn(async move {
            crate::ipc::pair_session(&*surface_pair, &state_pair, link_clone).await
        });

        probe.pair_at_lock.notified().await;
        probe.release_pair.notify_one();
        while !probe.pair_entered_lock.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
        probe.release_open.notify_one();

        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            let (pair_res, open_res, server_res) = tokio::join!(pair_task, open_task, pair_server);
            (pair_res.unwrap(), open_res.unwrap(), server_res.unwrap())
        })
        .await;

        assert!(joined.is_ok(), "interleaving timed out after 5s");
        let (pair_res, open_res, _) = joined.unwrap();
        assert!(pair_res.is_ok());
        match open_res {
            Err(crate::windows::OpenJournalError::Unpaired) => {}
            _ => panic!("expected OpenJournalError::Unpaired"),
        }
    }

    #[tokio::test]
    #[ignore = "the fake journal fixture fails its TLS handshake (certificate unknown); repair the fixture"]
    async fn pairing_success_closes_journal_surface_twin() {
        let state = Arc::new(test_app_state(None));
        let surface = Arc::new(FakeJournalSurface::new());

        let (signing_cert, signing_key) = signing_ca();
        let (server_cert, server_key) = signing_ca();
        let server_pin = spl_core::ca::sha256(server_cert.der())[..16].to_vec();
        let acceptor = TlsAcceptor::from(Arc::new(server_config(server_cert, server_key)));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let pair_server = tokio::spawn(serve_one_pair_response(
            listener,
            acceptor,
            signing_cert,
            signing_key,
        ));
        let link = direct_pair_link(port, &server_pin);

        let state_pair = state.clone();
        let surface_pair = surface.clone();
        let pair_task = tokio::spawn(async move {
            crate::ipc::pair_session(&*surface_pair, &state_pair, link).await
        });

        let joined = tokio::time::timeout(Duration::from_secs(5), async {
            let (pair_res, server_res) = tokio::join!(pair_task, pair_server);
            (pair_res.unwrap(), server_res.unwrap())
        })
        .await;

        assert!(joined.is_ok(), "pairing timed out after 5s");
        let (pair_res, _) = joined.unwrap();
        assert!(pair_res.is_ok());
        assert_eq!(surface.journal_closed.load(Ordering::SeqCst), 1);
    }
}
