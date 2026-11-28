// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The folder source, wired into the Windows app: paired protocol-v3 upload
//! path, journal facet/supported/estimate fetches, computer pause, overlap
//! discovery against Obsidian and screenshots, native Windows folder picker,
//! and IPC commands for the Settings window.
//!
//! All state, walks, sorting, and staging live in `folders-source`; Windows
//! metadata (`Zone.Identifier` ADS, WIC conversion, NTFS file IDs) lives in
//! `platform-win::folders`. Nothing here stores journal identity.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use folders_source::{
    outcome_for, parse_estimate, parse_facets, parse_supported_list, stage_delivery,
    BackfillEstimate, BoxFuture, Capability, DeliveryOutcome, EstimateItem, FacetOption,
    FacetsAvailability, FolderActionRequest, FolderDeclaration, FolderDeliveryReply,
    FolderPlatform, FolderTransport, FoldersHandle, Journal, OverlapContext, OverlapProvider,
    Paused, SegmentDelivery, SupportedList, CAPABILITY,
};
use observer_model::i18n::message;
use observer_model::{JournalLink, PairingPhase, PauseReason, ScreenshotsState};
use pl_transport_win::source_upload::{upload_folder_segment, FolderReply};
use pl_transport_win::{ObserverClient, TransportError};
use serde::Deserialize;
use tauri::Manager;

static HANDLE: OnceLock<FoldersHandle> = OnceLock::new();

type LoadClient = Arc<
    dyn Fn() -> Pin<Box<dyn Future<Output = Option<Arc<ObserverClient>>> + Send>> + Send + Sync,
>;
type Heard = Arc<dyn Fn() + Send + Sync>;
type StatusFlag = Arc<dyn Fn() -> bool + Send + Sync>;

/// The paired journal, over the app's framed mTLS upload path.
pub(crate) struct ClientJournal {
    load_client: LoadClient,
    platform: Arc<dyn FolderPlatform>,
    heard: Heard,
    paired_check: StatusFlag,
    offline_check: StatusFlag,
    asked: Mutex<Option<Arc<ObserverClient>>>,
}

impl ClientJournal {
    pub(crate) fn new(
        load_client: LoadClient,
        platform: Arc<dyn FolderPlatform>,
        heard: Heard,
        paired_check: StatusFlag,
        offline_check: StatusFlag,
    ) -> Self {
        Self {
            load_client,
            platform,
            heard,
            paired_check,
            offline_check,
            asked: Mutex::new(None),
        }
    }
}

impl Journal for ClientJournal {
    type Upload = SegmentDelivery;
    type Reply = Option<DeliveryOutcome>;

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

    fn deliver<'a>(
        &'a self,
        upload: &'a SegmentDelivery,
    ) -> BoxFuture<'a, Option<DeliveryOutcome>> {
        Box::pin(async move {
            let Some(client) = (self.load_client)().await else {
                return None;
            };
            if !self
                .asked
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .as_ref()
                .is_some_and(|asked| Arc::ptr_eq(asked, &client))
            {
                return None;
            }
            let delivery = upload.clone();
            let platform = Arc::clone(&self.platform);
            let staged =
                tokio::task::spawn_blocking(move || stage_delivery(&delivery, &*platform)).await;
            let (folder_meta, parts) = match staged {
                Ok(Ok(pair)) => pair,
                _ => return Some(DeliveryOutcome::Hold),
            };
            if !(self.load_client)()
                .await
                .is_some_and(|current| Arc::ptr_eq(&current, &client))
            {
                return Some(DeliveryOutcome::Hold);
            }
            let reply = upload_folder_segment(
                &client,
                &upload.window.day,
                &upload.window.segment,
                folder_meta,
                parts,
            )
            .await;
            delivery_for(reply, &self.heard)
        })
    }
}

impl FolderTransport for ClientJournal {
    fn is_paired(&self) -> bool {
        (self.paired_check)()
    }

    fn is_offline(&self) -> bool {
        (self.offline_check)()
    }

    fn fetch_facets(&self) -> BoxFuture<'_, (FacetsAvailability, Vec<FacetOption>)> {
        Box::pin(async move {
            if !self.is_paired() {
                return (FacetsAvailability::Unpaired, Vec::new());
            }
            let Some(client) = (self.load_client)().await else {
                return (FacetsAvailability::Unpaired, Vec::new());
            };
            match client.folder_facets().await {
                Ok(resp) if resp.status == 200 => {
                    (self.heard)();
                    match parse_facets(&resp.body) {
                        Some(list) => (FacetsAvailability::Ready, list),
                        None => (FacetsAvailability::Offline, Vec::new()),
                    }
                }
                Ok(_) => {
                    (self.heard)();
                    (FacetsAvailability::Offline, Vec::new())
                }
                Err(_) => (FacetsAvailability::Offline, Vec::new()),
            }
        })
    }

    fn fetch_supported(&self) -> BoxFuture<'_, Option<SupportedList>> {
        Box::pin(async move {
            let client = (self.load_client)().await?;
            match client.folder_supported().await {
                Ok(resp) if resp.status == 200 => {
                    (self.heard)();
                    parse_supported_list(&resp.body)
                }
                Ok(_) => {
                    (self.heard)();
                    None
                }
                Err(_) => None,
            }
        })
    }

    fn fetch_estimate(
        &self,
        folder_id: String,
        items: Vec<EstimateItem>,
    ) -> BoxFuture<'_, Option<BackfillEstimate>> {
        Box::pin(async move {
            let client = (self.load_client)().await?;
            let payload = serde_json::json!({
                "folder_id": folder_id,
                "items": items,
            });
            let body = serde_json::to_vec(&payload).ok()?;
            match client.folder_estimate(&body).await {
                Ok(resp) if resp.status == 200 => {
                    (self.heard)();
                    parse_estimate(&resp.body)
                }
                Ok(_) => {
                    (self.heard)();
                    None
                }
                Err(_) => None,
            }
        })
    }
}

fn delivery_for(reply: FolderReply, heard: &Heard) -> Option<DeliveryOutcome> {
    match reply {
        FolderReply::Answered {
            http_status,
            status,
            reason_code,
            refused_paths,
        } => {
            heard();
            Some(outcome_for(&FolderDeliveryReply {
                http_status,
                status,
                reason_code,
                refused_paths,
            }))
        }
        FolderReply::Held | FolderReply::Unreachable => Some(DeliveryOutcome::Hold),
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[derive(Deserialize)]
struct ObsidianSourceFile {
    #[serde(default)]
    enabled: bool,
    #[serde(default)]
    vault_root: Option<String>,
    #[serde(default)]
    vault_name: Option<String>,
}

fn current_overlap_context() -> OverlapContext {
    let obsidian_file = platform_win::local_data_root()
        .join("obsidian")
        .join("source.json");
    let obsidian = std::fs::read(&obsidian_file)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<ObsidianSourceFile>(&bytes).ok())
        .filter(|src| src.enabled)
        .and_then(|src| {
            let root = src.vault_root.filter(|p| !p.is_empty())?;
            let path = PathBuf::from(&root);
            let name = src
                .vault_name
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| folders_source::classify::folder_name(&path));
            Some((path, name))
        });

    let screenshots = crate::screenshots::source_status().and_then(|st| {
        if matches!(st.state, ScreenshotsState::Off | ScreenshotsState::NotSetUp) {
            return None;
        }
        let folder = st.folder.filter(|p| !p.is_empty())?;
        let path = PathBuf::from(&folder);
        let name = st
            .folder_name
            .filter(|n| !n.is_empty())
            .unwrap_or_else(|| folders_source::classify::folder_name(&path));
        Some((path, name))
    });

    OverlapContext {
        obsidian,
        screenshots,
    }
}

/// Open the folder source and start its background worker off the setup path.
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
    let heard: Heard = Arc::new(move || {
        let state = heard_app.state::<crate::app::AppState>();
        if let Ok(mut sync) = state.sync.lock() {
            sync.upload.record_answer(now_millis());
        };
    });

    let paired_app = app.clone();
    let paired_check: StatusFlag = Arc::new(move || {
        let state = paired_app.state::<crate::app::AppState>();
        state
            .sync
            .lock()
            .map(|s| s.pairing.phase == PairingPhase::Paired)
            .unwrap_or(false)
    });

    let offline_app = app.clone();
    let offline_check: StatusFlag = Arc::new(move || {
        if !platform_win::network_available() {
            return true;
        }
        let state = offline_app.state::<crate::app::AppState>();
        state
            .sync
            .lock()
            .map(|s| {
                s.pairing.phase != PairingPhase::Paired
                    || matches!(s.journal_link, Some(JournalLink::Offline))
            })
            .unwrap_or(true)
    });

    // Only the owner's pause holds delivery; scanning continues without skipping.
    let paused_app = app.clone();
    let paused: Paused = Arc::new(move || {
        let state = paused_app.state::<crate::app::AppState>();
        state
            .health
            .lock()
            .map(|h| matches!(h.pause, Some(p) if p.reason == PauseReason::Operator))
            .unwrap_or(false)
    });

    let overlap_provider: OverlapProvider = Arc::new(current_overlap_context);
    let platform: Arc<dyn FolderPlatform> = Arc::new(platform_win::folders::WindowsFolders);
    let journal = Arc::new(ClientJournal::new(
        load_client,
        Arc::clone(&platform),
        heard,
        paired_check,
        offline_check,
    ));
    let state_dir = platform_win::local_data_root().join("folders");
    let (handle, work) = folders_source::start(
        state_dir,
        platform,
        journal,
        paused,
        overlap_provider,
        counters,
    );
    let _ = HANDLE.set(handle);
    tauri::async_runtime::spawn(work);
}

fn handle() -> Option<&'static FoldersHandle> {
    HANDLE.get()
}

/// The owner confirmed a newly paired journal: restart setup on active/paused folders.
pub fn restart_setup() {
    if let Some(h) = handle() {
        h.restart_setup();
    }
}

/// Nudge the worker immediately (for example, when whole-computer pause toggles).
pub fn wake() {
    if let Some(h) = handle() {
        h.wake();
    }
}

/// Monotonic change receiver for the tray and shared-sources publisher.
pub fn state_changes() -> Option<tokio::sync::watch::Receiver<u64>> {
    handle().map(FoldersHandle::state_changes)
}

/// Per-folder declarations for `PUT /app/network/api/clients/self/sources`.
pub(crate) fn declared_folders() -> Vec<FolderDeclaration> {
    let Some(h) = handle() else {
        return Vec::new();
    };
    if h.document().journal_lists == Some(false) {
        return Vec::new();
    }
    h.declared_folders()
}

/// Whether any configured folder currently needs attention (missing or unreadable).
pub fn needs_attention() -> bool {
    handle().is_some_and(FoldersHandle::needs_attention)
}

/// Current folders status snapshot serialized for the Settings window.
pub fn status() -> Option<serde_json::Value> {
    let h = handle()?;
    serde_json::to_value(h.document()).ok()
}

/// Open the native Windows folder picker (`IFileOpenDialog`, `FOS_PICKFOLDERS`).
/// Returns `Ok(None)` when cancelled.
pub async fn pick_folder(window: tauri::WebviewWindow) -> Result<Option<String>, String> {
    crate::logs::require_settings(&window)?;
    let owner = window.hwnd().map_err(|_| "folders_window_unavailable")?.0 as isize;
    let chosen = tauri::async_runtime::spawn_blocking(move || {
        platform_win::folder_picker::choose_folder(
            owner,
            &message("windows.folders.pickertitle", &[]),
        )
    })
    .await
    .map_err(|_| "folders_dialog_failed")?
    .map_err(|_| "folders_dialog_failed")?;
    let Some(path) = chosen else {
        return Ok(None);
    };
    let Some(path) = path.to_str().map(str::to_owned) else {
        return Err("folders_unusable_path".into());
    };
    Ok(Some(path))
}

/// Validate a candidate path for readability, duplicates, and overlaps.
pub async fn check_path(path: String) -> Option<serde_json::Value> {
    let h = handle()?;
    serde_json::to_value(h.check_path(path).await).ok()
}

/// Execute one folder action and return the updated `FoldersDocument`.
pub async fn action(req: FolderActionRequest) -> Option<serde_json::Value> {
    let h = handle()?;
    serde_json::to_value(h.action(req).await).ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    use folders_source::{Engine, Span, ZoneIdentifier};
    use pl_transport_win::credential::{Credential, EndpointAddr};
    use rcgen::{CertificateParams, KeyPair, PKCS_ECDSA_P256_SHA256};
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    use rustls::ServerConfig;
    use spl_core::frame::{Frame, FrameDecoder, FLAG_CLOSE, FLAG_DATA};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio_rustls::TlsAcceptor;

    struct StubPlatform {
        ms: i64,
        home: PathBuf,
    }

    impl FolderPlatform for StubPlatform {
        fn now_ms(&self) -> i64 {
            self.ms
        }
        fn offset_s_at(&self, _epoch_secs: i64) -> i32 {
            -7 * 3600
        }
        fn home_dir(&self) -> PathBuf {
            self.home.clone()
        }
        fn file_ids_in_dir(&self, _dir: &Path) -> HashMap<OsString, String> {
            HashMap::new()
        }
        fn read_zone_identifier(&self, _root: &Path, _path: &Path) -> Option<ZoneIdentifier> {
            Some(ZoneIdentifier {
                zone_id: Some(3),
                origin_url: Some("https://example.org/file.md".into()),
                referrer_url: None,
            })
        }
    }

    fn self_signed() -> (CertificateDer<'static>, PrivateKeyDer<'static>) {
        let key = KeyPair::generate_for(&PKCS_ECDSA_P256_SHA256).unwrap();
        let params = CertificateParams::new(vec!["spl.local".to_string()]).unwrap();
        let cert = params.self_signed(&key).unwrap();
        (
            CertificateDer::from(cert.der().to_vec()),
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der())),
        )
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

    #[tokio::test]
    async fn folder_wire_routes_and_ingest_deliver_end_to_end() {
        let dir = tempfile::tempdir().unwrap();
        let folder = dir.path().join("Downloads");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("note.md"), b"# Hello\nfrom windows\n").unwrap();

        let now_ms = std::time::SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_millis() as i64
            + 600_000;
        let platform: Arc<dyn FolderPlatform> = Arc::new(StubPlatform {
            ms: now_ms,
            home: dir.path().to_path_buf(),
        });

        let mut engine = Engine::open(dir.path().join("state"), Arc::clone(&platform));
        let id = engine.begin_setup(&folder, None).unwrap();
        engine.confirm_folder(&id, Span::All).unwrap();
        let delivery = engine.next_delivery(false).unwrap();

        let (client, requests) = start_journal(vec![
            (200, r#"{"capabilities":["folder"]}"#.into()),
            (200, r#"{"facets":[{"name":"work","title":"Work"}]}"#.into()),
            (
                200,
                r#"{"version":1,"ruleset_version":8,"kinds":{"notes":{"extensions":["md"]}}}"#
                    .into(),
            ),
            (200, r#"{"segments":1,"days":1,"thoughts":1}"#.into()),
            (
                200,
                r#"{"status":"ok","segment":"140500_300","file_descriptors":[]}"#.into(),
            ),
        ])
        .await;

        let heard_count = Arc::new(AtomicUsize::new(0));
        let heard_c = Arc::clone(&heard_count);
        let load: LoadClient = Arc::new(move || {
            let c = Arc::clone(&client);
            Box::pin(async move { Some(c) })
        });
        let journal = ClientJournal::new(
            load,
            platform,
            Arc::new(move || {
                heard_c.fetch_add(1, Ordering::SeqCst);
            }),
            Arc::new(|| true),
            Arc::new(|| false),
        );

        assert_eq!(journal.capability().await, Capability::Supported);
        let (avail, facets) = journal.fetch_facets().await;
        assert_eq!(avail, FacetsAvailability::Ready);
        assert_eq!(facets.len(), 1);
        assert!(journal.fetch_supported().await.is_some());
        let est = journal.fetch_estimate(id.clone(), vec![]).await.unwrap();
        assert_eq!(est.segments, 1);
        assert_eq!(
            journal.deliver(&delivery).await,
            Some(DeliveryOutcome::Delivered)
        );
        assert_eq!(heard_count.load(Ordering::SeqCst), 5);

        let seen = requests.lock().unwrap();
        let ingest_body = String::from_utf8_lossy(&seen[4]);
        assert!(ingest_body.contains(r#""source":"folder""#));
        assert!(ingest_body.contains(r#""zone_id":3"#));
        assert!(ingest_body.contains("c_01.md"));
        assert!(ingest_body.contains("folder_"));
    }
}
