// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The browser extension's native host, wired into the app.
//!
//! [`dispatch`] runs first in `main`, before Velopack's hooks or any other
//! init: a browser's native-messaging launch either becomes the host relay
//! (an allowlisted extension, in a build with `browser-host`) or exits at once,
//! and never starts the tray app.
//!
//! In the running app, [`start`] binds the same-user pipe, writes the browsers'
//! registration for the installed copy, feeds the capture gate from pairing and
//! pause, and delivers finalized periods as the journal's `browser` source.
//! Everything here is compiled out unless the `browser-host` feature is on; no
//! release target sets it until the browser extension launches.

use std::process::ExitCode;

/// Handle a native-messaging launch. `None` for every other launch.
pub fn dispatch(args: &[String]) -> Option<ExitCode> {
    if !observer_model::is_native_messaging_launch(args) {
        return None;
    }
    #[cfg(feature = "browser-host")]
    {
        Some(imp::host_main(args))
    }
    #[cfg(not(feature = "browser-host"))]
    {
        // Host mode is not in this build: refuse, open nothing.
        Some(ExitCode::from(2))
    }
}

#[cfg(feature = "browser-host")]
pub use imp::*;

#[cfg(feature = "browser-host")]
mod imp {
    use std::path::PathBuf;
    use std::process::ExitCode;
    use std::sync::{Arc, OnceLock};
    use std::time::{Duration, SystemTime, UNIX_EPOCH};

    use browser_host::argv::{classify, Classified};
    use browser_host::custody::{OutboxEntry, Policy, Store};
    use browser_host::hub::{BrowserStatus, Gates, Hub, HubConfig, Pairing};
    use browser_host::relay::{self, RelayEnd};
    use browser_host::upload::{deliver_pending, Journal, UploadOutcome};
    use observer_model::{AppPhase, LocalOffset, PairingPhase};
    use pl_transport_win::source_upload::{upload_source_file, SourceFile, SourceUploadOutcome};
    use pl_transport_win::ObserverClient;
    use tauri::Manager;

    /// The development ids are admitted only in a build that asks for them.
    pub const DEVELOPMENT: bool = cfg!(feature = "browser-dev-host");

    static HUB: OnceLock<Arc<Hub>> = OnceLock::new();

    fn now_ms() -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0)
    }

    pub fn intake_root() -> PathBuf {
        platform_win::local_data_root().join("browser-intake")
    }

    fn status_path() -> PathBuf {
        intake_root().join("status.json")
    }

    // --- host mode ---------------------------------------------------------

    /// The host relay process. Never logs page content; writes only frames to
    /// stdout.
    pub fn host_main(args: &[String]) -> ExitCode {
        let Classified::Host(invocation) = classify(args, DEVELOPMENT) else {
            return ExitCode::from(2);
        };
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
        else {
            return ExitCode::from(1);
        };
        let end = runtime.block_on(async {
            let pipe = match platform_win::browser_pipe::pipe_name() {
                Ok(name) => platform_win::browser_pipe::connect(&name)
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            };
            relay::run(invocation, tokio::io::stdin(), tokio::io::stdout(), pipe).await
        });
        // stdin is read on a blocking thread the runtime would wait for.
        std::process::exit(match end {
            RelayEnd::AppAbsent
            | RelayEnd::BrowserClosed
            | RelayEnd::AppClosed
            | RelayEnd::Finished => 0,
            RelayEnd::HandshakeTimeout | RelayEnd::Fault => 1,
        })
    }

    // --- registration ------------------------------------------------------

    /// Write (or repair) the browsers' registration for `exe`, and remove the
    /// development rows a build without the development host must not leave.
    pub fn ensure_registration(exe: &std::path::Path) {
        let exe = exe.to_string_lossy();
        let dir = platform_win::browser_registration::manifests_dir();
        let wanted = browser_host::registration::wanted(&exe, DEVELOPMENT);
        let rows: Vec<_> = wanted
            .iter()
            .map(|r| platform_win::browser_registration::Row {
                registry_key: &r.registry_key,
                manifest_file: &r.manifest_file,
                json: &r.json,
            })
            .collect();
        match platform_win::browser_registration::ensure(&dir, &rows) {
            Ok(outcome) => tracing::info!(
                target: "browser",
                component = "registration",
                written = outcome.written,
                already_current = outcome.already_current,
                "browser registration ensure"
            ),
            Err(error) => tracing::warn!(
                target: "browser",
                component = "registration",
                outcome = "failed",
                error = %error,
                "browser registration ensure"
            ),
        }
        let unwanted = browser_host::registration::unwanted(&exe, DEVELOPMENT);
        let rows: Vec<_> = unwanted
            .iter()
            .map(|r| platform_win::browser_registration::Row {
                registry_key: &r.registry_key,
                manifest_file: &r.manifest_file,
                json: &r.json,
            })
            .collect();
        let _ = platform_win::browser_registration::remove(&dir, &rows);
    }

    /// Remove every registration row this app wrote (the uninstall callback).
    pub fn remove_registration(exe: &std::path::Path) {
        let exe = exe.to_string_lossy();
        let dir = platform_win::browser_registration::manifests_dir();
        let mut all = browser_host::registration::wanted(&exe, true);
        all.extend(browser_host::registration::unwanted(&exe, false));
        let rows: Vec<_> = all
            .iter()
            .map(|r| platform_win::browser_registration::Row {
                registry_key: &r.registry_key,
                manifest_file: &r.manifest_file,
                json: &r.json,
            })
            .collect();
        let _ = platform_win::browser_registration::remove(&dir, &rows);
    }

    // --- the running app ---------------------------------------------------

    fn local_zone(epoch_secs: u64) -> Option<observer_model::LocalZone> {
        platform_win::WindowsLocalOffset.local_zone(epoch_secs).ok()
    }

    fn period_keys(start_secs: u64, len_secs: u64, offset: i64) -> (String, String) {
        (
            observer_pl_civil::day(start_secs, offset),
            observer_pl_civil::segment(start_secs, offset, len_secs),
        )
    }

    fn native_about_snapshot(app: &tauri::AppHandle) -> observer_model::about::NativeAboutSnapshot {
        let state = app.state::<crate::app::AppState>();
        let sync = state
            .sync
            .lock()
            .map(|snapshot| snapshot.clone())
            .unwrap_or_default();
        observer_model::about::native_windows_snapshot(
            &state.about_observation,
            &sync.journal_base_line,
            sync.journal_version_fresh,
            sync.journal_seen_at_epoch_secs,
        )
    }

    /// Bind the endpoint, open custody and start the gate, clock and delivery
    /// tasks. Called once from setup, after the single-instance gate.
    pub fn start(app: tauri::AppHandle, installed_exe: Option<PathBuf>) {
        if let Some(exe) = installed_exe {
            ensure_registration(&exe);
        }
        let namer: browser_host::custody::Namer = Box::new(|start, len| {
            let offset = local_zone(start).map_or(0, |z| z.utc_offset_seconds);
            period_keys(start, len, offset)
        });
        let store = Store::open(intake_root(), Policy::default(), namer, now_ms());
        let hub = Hub::new(
            HubConfig {
                development: DEVELOPMENT,
                app_version: env!("CARGO_PKG_VERSION").to_string(),
                about: native_about_snapshot(&app),
            },
            store,
            Box::new(now_ms),
        );
        let _ = HUB.set(Arc::clone(&hub));

        // The endpoint: first instance only; a name someone else holds is a
        // collision we refuse, not a pipe we share.
        let server_hub = Arc::clone(&hub);
        tauri::async_runtime::spawn(async move {
            let name = match platform_win::browser_pipe::pipe_name() {
                Ok(name) => name,
                Err(error) => {
                    tracing::warn!(target: "browser", component = "endpoint", outcome = "name_failed", error = %error, "browser endpoint");
                    return;
                }
            };
            let mut server = match platform_win::browser_pipe::create_server(&name, true) {
                Ok(server) => server,
                Err(error) => {
                    let outcome = if platform_win::browser_pipe::is_collision(&error) {
                        "collision"
                    } else {
                        "bind_failed"
                    };
                    tracing::warn!(target: "browser", component = "endpoint", outcome, error = %error, "browser endpoint");
                    return;
                }
            };
            tracing::info!(target: "browser", component = "endpoint", outcome = "listening", "browser endpoint");
            loop {
                if let Err(error) = server.connect().await {
                    tracing::warn!(target: "browser", component = "endpoint", outcome = "accept_failed", error = %error, "browser endpoint");
                    tokio::time::sleep(Duration::from_millis(250)).await;
                    continue;
                }
                let next = match platform_win::browser_pipe::create_server(&name, false) {
                    Ok(next) => next,
                    Err(error) => {
                        tracing::warn!(target: "browser", component = "endpoint", outcome = "rebind_failed", error = %error, "browser endpoint");
                        return;
                    }
                };
                let connected = std::mem::replace(&mut server, next);
                tokio::spawn(Arc::clone(&server_hub).serve(connected));
            }
        });

        // Gates and the period clock, once a second.
        let gate_app = app.clone();
        let gate_hub = Arc::clone(&hub);
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(1));
            let mut published = Vec::new();
            loop {
                tick.tick().await;
                let gates = read_gates(&gate_app).await;
                gate_hub.update_gates(gates);
                gate_hub.update_about_snapshot(native_about_snapshot(&gate_app));
                gate_hub.tick();
                write_status(&gate_hub.status(), &mut published);
            }
        });

        // Delivery, every few seconds while anything is held.
        let upload_app = app;
        let upload_hub = hub;
        tauri::async_runtime::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(5));
            loop {
                tick.tick().await;
                if upload_hub.outbox().is_empty() {
                    continue;
                }
                let pass = deliver_pending(&upload_hub, || async {
                    current_client(&upload_app).await.map(ClientJournal::new)
                })
                .await;
                if pass.delivered > 0 {
                    tracing::info!(target: "browser", component = "delivery", delivered = pass.delivered, remaining = pass.remaining, "browser delivery");
                }
            }
        });
    }

    async fn current_client(app: &tauri::AppHandle) -> Option<Arc<ObserverClient>> {
        let state = app.state::<crate::app::AppState>();
        pl_transport_win::access::load_current_client(&state.credential_access).await
    }

    async fn read_gates(app: &tauri::AppHandle) -> Gates {
        let state = app.state::<crate::app::AppState>();
        let paused = state
            .health
            .lock()
            .map(|h| h.app_state == AppPhase::Paused)
            .unwrap_or(false);
        let phase = state
            .sync
            .lock()
            .map(|s| s.pairing.phase)
            .unwrap_or(PairingPhase::NotPaired);
        let pairing = match phase {
            PairingPhase::Paired | PairingPhase::AwaitingConfirmation => {
                let identity = current_client(app).await.and_then(|c| {
                    let credential = c.credential();
                    browser_host::identity::journal_identity_from_pem(
                        &credential.instance_id,
                        &credential.ca_chain_pem,
                    )
                });
                Pairing::Paired { identity }
            }
            PairingPhase::NotPaired | PairingPhase::Pairing | PairingPhase::Failed => {
                Pairing::NotPaired
            }
        };
        Gates { pairing, paused }
    }

    /// One view of the client that will carry an upload.
    struct ClientJournal {
        client: Arc<ObserverClient>,
    }

    impl ClientJournal {
        fn new(client: Arc<ObserverClient>) -> Self {
            Self { client }
        }
    }

    impl Journal for ClientJournal {
        fn same_connection(&self, current: &Self) -> bool {
            Arc::ptr_eq(&self.client, &current.client)
        }

        async fn upload(&self, entry: &OutboxEntry, body: Vec<u8>) -> UploadOutcome {
            let Some(zone) = local_zone(entry.start_secs) else {
                return UploadOutcome::Failed("local_io");
            };
            let (day, segment) =
                period_keys(entry.start_secs, entry.len_secs, zone.utc_offset_seconds);
            let outcome = upload_source_file(
                &self.client,
                SourceFile {
                    source: browser_host::SOURCE,
                    day: &day,
                    segment: &segment,
                    zone: &zone,
                    filename: browser_host::PAGES_FILE,
                    content_type: browser_host::PAGES_CONTENT_TYPE,
                    bytes: body,
                },
            )
            .await;
            match outcome {
                SourceUploadOutcome::Stored => UploadOutcome::Delivered,
                SourceUploadOutcome::Held => UploadOutcome::Held,
                SourceUploadOutcome::Unreachable => UploadOutcome::Failed("relay_unavailable"),
                SourceUploadOutcome::Rejected => UploadOutcome::Failed("journal_rejected"),
            }
        }
    }

    // --- status, owner actions, update -------------------------------------

    /// Velopack's startup step (`VelopackApp::run`) applies a downloaded update
    /// before it returns, and the apply kills every process under `current\`,
    /// browser hosts included. Every launch of this exe passes through it
    /// (`--apply-update`, a relaunch, a second launch), so `main` calls this
    /// first when an apply is pending: the running app is asked to send its
    /// hosts away (see [`request_quiesce_from_running_app`]), as the in-app
    /// install does. Prints what happened; the apply goes ahead either way.
    pub fn quiesce_before_apply() {
        let line = match request_quiesce_from_running_app() {
            Ok(closed) => format!("browser hosts quiesced (all closed: {closed})"),
            Err(reason) => format!("browser hosts not quiesced: {reason}"),
        };
        println!("update: {line}");
        // No file log exists this early, and a GUI-subsystem console can drop
        // stdout: leave the outcome where an operator can read it afterwards.
        let _ = std::fs::write(
            intake_root().join("update-quiesce.txt"),
            format!("{} {line}\n", now_ms()),
        );
    }

    /// The status the app last published (counts and states only; never page
    /// text). Read by `--browser-status` and the Settings row.
    fn write_status(status: &BrowserStatus, published: &mut Vec<u8>) {
        let Ok(bytes) = serde_json::to_vec_pretty(status) else {
            return;
        };
        if *published == bytes {
            return;
        }
        published.clone_from(&bytes);
        let path = status_path();
        let tmp = path.with_extension("json.partial");
        if std::fs::write(&tmp, bytes).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    }

    pub fn status() -> Option<BrowserStatus> {
        HUB.get().map(|hub| hub.status())
    }

    /// `--browser-status`: print what the running app last published.
    pub fn status_cli() -> ExitCode {
        match std::fs::read_to_string(status_path()) {
            Ok(text) => {
                println!("{}", text.trim_end());
                ExitCode::SUCCESS
            }
            Err(error) => {
                eprintln!("--browser-status: no status published ({error})");
                ExitCode::FAILURE
            }
        }
    }

    /// The owner discards browser pages still waiting to be sent.
    pub fn discard_waiting() -> Option<usize> {
        HUB.get().map(|hub| hub.discard_waiting())
    }

    /// Before the in-app updater applies: say `bye(update)` to every connected
    /// host and wait for them to leave. Returns whether they all did.
    pub fn quiesce_for_update() -> bool {
        match HUB.get() {
            Some(hub) => {
                let hub = Arc::clone(hub);
                let closed = tauri::async_runtime::block_on(async move {
                    hub.quiesce(browser_host::hub::QUIESCE_WAIT).await
                });
                tracing::info!(target: "update", component = "browser", closed, "browser host quiescence");
                closed
            }
            None => true,
        }
    }

    /// The update did not apply after all.
    pub fn resume_after_update() {
        if let Some(hub) = HUB.get() {
            hub.resume();
        }
    }

    /// `--apply-update` runs as its own process: ask the running app, over its
    /// own pipe, to quiesce first. `Err` names why it could not be asked.
    pub fn request_quiesce_from_running_app() -> Result<bool, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("runtime: {e}"))?;
        runtime.block_on(async {
            let name =
                platform_win::browser_pipe::pipe_name().map_err(|e| format!("pipe name: {e}"))?;
            let pipe = match platform_win::browser_pipe::connect(&name).await {
                Ok(Some(pipe)) => pipe,
                Ok(None) => return Err("no running app endpoint".to_string()),
                Err(e) => return Err(format!("connect: {e}")),
            };
            relay::request_quiesce(pipe)
                .await
                .ok_or_else(|| "no reply from the running app".to_string())
        })
    }

    /// Civil day and segment keys, the same functions the capture uploader uses.
    mod observer_pl_civil {
        pub fn day(start_secs: u64, offset: i64) -> String {
            pl_transport_win::civil::day_string_local(start_secs, offset)
        }

        pub fn segment(start_secs: u64, offset: i64, len_secs: u64) -> String {
            pl_transport_win::civil::segment_key_string_local(start_secs, offset, len_secs)
        }
    }
}
