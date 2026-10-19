// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The shared automation contract — code, not prose.
//!
//! Two vocabularies live here as the single source of truth:
//!
//!  1. **AutomationId identifiers** — the namespaced `data-automation-id` /
//!     UIA AutomationId strings the FlaUI harness and the webview both reference
//!     (`tray.menu.pause`, `settings.status.appState.state`, …). Declared as
//!     `const`s below so a rename is a compile-time event.
//!  2. **State/source token vocabulary** — the serialized enum tokens
//!     (`idle`/`observing`/…, `screen`/`system_audio`/…, `active`/
//!     `no_input_device`/…). These are **derived from the `observer-model`
//!     enums** via [`strum::IntoEnumIterator`], so you cannot add a
//!     `SourceState` variant without the contract noticing.
//!
//! [`generate_contract`] renders both into one deterministic JSON document
//! (sorted keys, pretty, trailing newline) — the committed
//! `automation-contract.json` at the repo root. `cargo xtask contract` writes
//! it; `cargo xtask contract --check` regenerates in memory and diffs.

#![forbid(unsafe_code)]

use std::collections::BTreeMap;

pub use observer_model::setup::SETUP_SEQUENCE_ENABLED;
use observer_model::{
    AppPhase, ErrorReason, PairingPhase, PauseReason, SourceKind, ViewRenderState,
    RELOAD_SETUP_VERB, SETUP_HANDOFF_ARG,
};

use serde::Serialize;
use strum::IntoEnumIterator;

/// Header value pinned as the first key of the generated artifact.
pub const GENERATED_BANNER: &str = "DO NOT EDIT — run make contract";

/// Fixed loopback ports outside the default Windows dynamic range.
pub const LOOPBACK_HOST: &str = "127.0.0.1";
pub const HEALTH_PORT: u16 = 47247;
pub const HEALTH_PATH: &str = "/healthz";
pub const CONTROL_PORT: u16 = 47248;

// ── AutomationId source of truth ─────────────────────────────────────────────
// Namespaced identifiers. Renaming any of these is a compile-time break for
// every consumer that imports the const, and a contract-diff for the harness.

/// Tray surface.
pub mod tray {
    pub const ROOT: &str = "tray.root";
    /// The Pause submenu root; its children are the duration choices below.
    pub const MENU_PAUSE: &str = "tray.menu.pause";
    pub const MENU_PAUSE_15M: &str = "tray.menu.pause15m";
    pub const MENU_PAUSE_30M: &str = "tray.menu.pause30m";
    pub const MENU_PAUSE_1H: &str = "tray.menu.pause1h";
    pub const MENU_PAUSE_INDEFINITE: &str = "tray.menu.pauseIndefinite";
    pub const MENU_RESUME: &str = "tray.menu.resume";
    pub const MENU_RESTART_OBSERVING: &str = "tray.menu.restartObserving";
    pub const MENU_BROWSERS: &str = "tray.menu.browsers";
    pub const MENU_OPEN_JOURNAL: &str = "tray.menu.openJournal";
    pub const MENU_OPEN_SETTINGS: &str = "tray.menu.openSettings";
    pub const MENU_GET_HELP: &str = "tray.menu.getHelp";
    pub const MENU_REPORT_PROBLEM: &str = "tray.menu.reportProblem";
    pub const MENU_ABOUT: &str = "tray.menu.about";
    pub const MENU_QUIT: &str = "tray.menu.quit";
}

/// Settings window.
pub mod settings {
    pub const WINDOW_ROOT: &str = "settings.window.root";
    /// Home pane: the first-run kinship intro block (shown only while not paired).
    pub const HOME_KINSHIP: &str = "settings.home.kinship";
    pub const LOGS_READ: &str = "settings.logs.read";
    pub const LOGS_PREVIEW: &str = "settings.logs.preview";
    pub const LOGS_SAVE: &str = "settings.logs.save";
    pub const STATUS_APP_STATE: &str = "settings.status.appState.state";
    pub const STATUS_SEGMENT_DIR: &str = "settings.status.segmentDir";
    pub const STATUS_UPLOAD_STATE: &str = "settings.status.upload.state";
    pub const SOURCES_SCREEN_STATE: &str = "settings.sources.screen.state";
    pub const SOURCES_SCREEN_ENABLED: &str = "settings.sources.screen.enabled";
    pub const SOURCES_SYSTEM_AUDIO_STATE: &str = "settings.sources.systemAudio.state";
    pub const SOURCES_SYSTEM_AUDIO_ENABLED: &str = "settings.sources.systemAudio.enabled";
    pub const SOURCES_MIC_STATE: &str = "settings.sources.mic.state";
    pub const SOURCES_MIC_ENABLED: &str = "settings.sources.mic.enabled";
    pub const SOURCES_DESKTOP_ACTIVITY_STATE: &str = "settings.sources.desktopActivity.state";
    pub const SOURCES_DESKTOP_ACTIVITY_ENABLED: &str = "settings.sources.desktopActivity.enabled";
    pub const SOURCES_DESKTOP_ACTIVITY_WINDOW_TITLES: &str =
        "settings.sources.desktopActivity.windowTitles";
    pub const SOURCES_DESKTOP_ACTIVITY_PAGE_ADDRESSES: &str =
        "settings.sources.desktopActivity.pageAddresses";
    pub const SOURCES_SAVE_RETRY: &str = "settings.sources.saveRetry";
    pub const SOURCES_BROWSER_OFFER: &str = "settings.sources.browser.offer";
    pub const SOURCES_BROWSER_STORES: &str = "settings.sources.browser.stores";
    pub const SOURCES_BROWSER_STORE_ERROR: &str = "settings.sources.browser.storeError";

    pub const SETUP_STEP: &str = "settings.setup.step";
    pub const SETUP_NEXT: &str = "settings.setup.next";
    pub const SETUP_BACK: &str = "settings.setup.back";
    pub const SETUP_NOT_NOW: &str = "settings.setup.notNow";
    pub const SETUP_START_AT_LOGIN: &str = "settings.setup.startAtLogin";
    pub const SETUP_START_AT_LOGIN_ERROR: &str = "settings.setup.startAtLogin.error";
    pub const SETUP_MIC_PROOF: &str = "settings.setup.mic.proof";
    pub const SETUP_SYSTEM_AUDIO_PROOF: &str = "settings.setup.systemAudio.proof";
    pub const SETUP_SCREEN_PROOF: &str = "settings.setup.screen.proof";
    pub const SETUP_STEP_NUMBER: &str = "settings.setup.stepNumber";
    pub const SETUP_FRESH_LINK: &str = "settings.setup.freshLink";
    pub const SETUP_LINK_RETRY: &str = "settings.setup.linkRetry";
    pub const SETUP_NATIVE_CONNECT: &str = "settings.setup.nativeConnect";
    pub const SETUP_DEFERRED_USE: &str = "settings.setup.deferredUse";
    pub const SETUP_DEFERRED_DISMISS: &str = "settings.setup.deferredDismiss";

    pub const HOME_SOURCES_NOTICE: &str = "settings.home.sourcesNotice";
    pub const HOME_SOURCES_NOTICE_DISMISS: &str = "settings.home.sourcesNotice.dismiss";
    /// Pairing pane (Wave 2): phase, the journal's mark, and the pair-link field + action.
    pub const PAIRING_STATE: &str = "settings.pairing.state";
    pub const PAIRING_INPUT: &str = "settings.pairing.input";
    pub const PAIRING_SUBMIT: &str = "settings.pairing.submit";
    pub const PAIRING_FINISH_CONNECTING: &str = "settings.pairing.finishConnecting";
    pub const SETUP_BROWSER_JOURNAL: &str = "settings.setup.browserJournal";
    /// The mark row is the journal's identity whenever the confirmation card
    /// is not on screen (generic mark before the owner confirms, the real
    /// mark once paired, unavailable mark when paired but the mark cannot be
    /// read).
    pub const PAIRING_MARK: &str = "settings.pairing.mark";
    pub const PAIRING_MARK_CARD: &str = "settings.pairing.markCard";
    pub const PAIRING_CONFIRM_YES: &str = "settings.pairing.confirmYes";
    pub const PAIRING_CONFIRM_REJECT: &str = "settings.pairing.confirmReject";
    pub const PAIRING_CONFIRM_CONTINUE: &str = "settings.pairing.confirmContinue";
    pub const PAIRING_CONFIRM_CANCEL: &str = "settings.pairing.confirmCancel";
    pub const JOURNAL_OPEN: &str = "settings.journal.open";
    pub const JOURNAL_UNAVAILABLE: &str = "settings.journal.unavailable";
    pub const UNKNOWN_JOURNAL_LIST: &str = "settings.unknown-journal.list";
    pub const UNKNOWN_JOURNAL_SUMMARY: &str = "settings.unknown-journal.summary";
    pub const UNKNOWN_JOURNAL_DETAIL: &str = "settings.unknown-journal.detail";
    pub const UNKNOWN_JOURNAL_WHAT_ANSWERED: &str = "settings.unknown-journal.what-answered";
    pub const UNKNOWN_JOURNAL_YOUR_JOURNAL: &str = "settings.unknown-journal.your-journal";
    pub const UNKNOWN_JOURNAL_RESPONDING_CAPTION: &str =
        "settings.unknown-journal.responding-caption";
    /// Updates pane: honest state line, live last-checked, the control buttons,
    /// the auto-check / frequency / background-download settings, release notes.
    pub const UPDATES_STATE: &str = "settings.updates.state";
    pub const UPDATES_LAST_CHECKED: &str = "settings.updates.lastChecked";
    pub const UPDATES_CHECK_NOW: &str = "settings.updates.checkNow";
    pub const UPDATES_CANCEL: &str = "settings.updates.cancel";
    pub const UPDATES_DOWNLOAD: &str = "settings.updates.download";
    pub const UPDATES_INSTALL: &str = "settings.updates.install";
    pub const UPDATES_RETRY: &str = "settings.updates.retry";
    pub const UPDATES_DISMISS: &str = "settings.updates.dismiss";
    pub const UPDATES_AUTO_CHECK: &str = "settings.updates.autoCheck";
    pub const UPDATES_FREQUENCY: &str = "settings.updates.frequency";
    pub const UPDATES_AUTO_DOWNLOAD: &str = "settings.updates.autoDownload";
    pub const UPDATES_NOTES: &str = "settings.updates.notes";
    /// Exclusions pane (Privacy): the private-browsing toggle, the excluded-app
    /// picker + list, the title-pattern field + list, and the exclusion-activity
    /// line (redacted/dropped counts — exclusion is never silent).
    pub const EXCLUSIONS_PRIVATE_BROWSING: &str = "settings.exclusions.privateBrowsing";
    pub const EXCLUSIONS_APP_INPUT: &str = "settings.exclusions.appInput";
    pub const EXCLUSIONS_APP_ADD: &str = "settings.exclusions.appAdd";
    pub const EXCLUSIONS_APPS_LIST: &str = "settings.exclusions.appsList";
    pub const EXCLUSIONS_TITLE_INPUT: &str = "settings.exclusions.titleInput";
    pub const EXCLUSIONS_TITLE_ADD: &str = "settings.exclusions.titleAdd";
    pub const EXCLUSIONS_TITLES_LIST: &str = "settings.exclusions.titlesList";
    pub const EXCLUSIONS_ACTIVITY: &str = "settings.exclusions.activity";
    /// Global pause/resume hotkey pane: the on/off toggle, the current combo +
    /// capture/clear controls, and the honest registration status line (a combo
    /// another app already owns shows here, never a silent no-op).
    pub const HOTKEY_ENABLED: &str = "settings.hotkey.enabled";
    pub const HOTKEY_COMBO: &str = "settings.hotkey.combo";
    pub const HOTKEY_CLEAR: &str = "settings.hotkey.clear";
    pub const HOTKEY_STATUS: &str = "settings.hotkey.status";
    /// Microphones pane: the device priority/disable list, the active-device line,
    /// and the input-gain control.
    pub const MIC_DEVICES: &str = "settings.mic.devices";
    pub const MIC_ACTIVE: &str = "settings.mic.active";
    pub const MIC_GAIN: &str = "settings.mic.gain";
}

/// About window.
pub mod about {
    pub const WINDOW_ROOT: &str = "about.window.root";
    pub const VERSION: &str = "about.version";
    pub const JOURNAL_VERSION: &str = "about.journalVersion";
}

/// Every AutomationId const, paired with a stable contract key. The contract key
/// is the JSON property; the value is the AutomationId string consumers stamp.
fn automation_ids() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("tray.root", tray::ROOT),
        ("tray.menu.pause", tray::MENU_PAUSE),
        ("tray.menu.pause15m", tray::MENU_PAUSE_15M),
        ("tray.menu.pause30m", tray::MENU_PAUSE_30M),
        ("tray.menu.pause1h", tray::MENU_PAUSE_1H),
        ("tray.menu.pauseIndefinite", tray::MENU_PAUSE_INDEFINITE),
        ("tray.menu.resume", tray::MENU_RESUME),
        ("tray.menu.restartObserving", tray::MENU_RESTART_OBSERVING),
        ("tray.menu.browsers", tray::MENU_BROWSERS),
        ("tray.menu.openJournal", tray::MENU_OPEN_JOURNAL),
        ("tray.menu.openSettings", tray::MENU_OPEN_SETTINGS),
        ("tray.menu.getHelp", tray::MENU_GET_HELP),
        ("tray.menu.reportProblem", tray::MENU_REPORT_PROBLEM),
        ("tray.menu.about", tray::MENU_ABOUT),
        ("tray.menu.quit", tray::MENU_QUIT),
        ("settings.window.root", settings::WINDOW_ROOT),
        ("settings.home.kinship", settings::HOME_KINSHIP),
        ("settings.logs.read", settings::LOGS_READ),
        ("settings.logs.preview", settings::LOGS_PREVIEW),
        ("settings.logs.save", settings::LOGS_SAVE),
        ("settings.status.appState.state", settings::STATUS_APP_STATE),
        ("settings.status.segmentDir", settings::STATUS_SEGMENT_DIR),
        (
            "settings.status.upload.state",
            settings::STATUS_UPLOAD_STATE,
        ),
        (
            "settings.sources.screen.state",
            settings::SOURCES_SCREEN_STATE,
        ),
        (
            "settings.sources.screen.enabled",
            settings::SOURCES_SCREEN_ENABLED,
        ),
        (
            "settings.sources.systemAudio.state",
            settings::SOURCES_SYSTEM_AUDIO_STATE,
        ),
        (
            "settings.sources.systemAudio.enabled",
            settings::SOURCES_SYSTEM_AUDIO_ENABLED,
        ),
        ("settings.sources.mic.state", settings::SOURCES_MIC_STATE),
        (
            "settings.sources.mic.enabled",
            settings::SOURCES_MIC_ENABLED,
        ),
        (
            "settings.sources.desktopActivity.state",
            settings::SOURCES_DESKTOP_ACTIVITY_STATE,
        ),
        (
            "settings.sources.desktopActivity.enabled",
            settings::SOURCES_DESKTOP_ACTIVITY_ENABLED,
        ),
        (
            "settings.sources.desktopActivity.windowTitles",
            settings::SOURCES_DESKTOP_ACTIVITY_WINDOW_TITLES,
        ),
        (
            "settings.sources.desktopActivity.pageAddresses",
            settings::SOURCES_DESKTOP_ACTIVITY_PAGE_ADDRESSES,
        ),
        ("settings.sources.saveRetry", settings::SOURCES_SAVE_RETRY),
        ("settings.setup.step", settings::SETUP_STEP),
        ("settings.setup.next", settings::SETUP_NEXT),
        ("settings.setup.back", settings::SETUP_BACK),
        ("settings.setup.notNow", settings::SETUP_NOT_NOW),
        (
            "settings.setup.startAtLogin",
            settings::SETUP_START_AT_LOGIN,
        ),
        (
            "settings.setup.startAtLogin.error",
            settings::SETUP_START_AT_LOGIN_ERROR,
        ),
        ("settings.setup.mic.proof", settings::SETUP_MIC_PROOF),
        (
            "settings.setup.systemAudio.proof",
            settings::SETUP_SYSTEM_AUDIO_PROOF,
        ),
        ("settings.setup.screen.proof", settings::SETUP_SCREEN_PROOF),
        ("settings.setup.stepNumber", settings::SETUP_STEP_NUMBER),
        ("settings.setup.freshLink", settings::SETUP_FRESH_LINK),
        ("settings.setup.linkRetry", settings::SETUP_LINK_RETRY),
        (
            "settings.setup.nativeConnect",
            settings::SETUP_NATIVE_CONNECT,
        ),
        ("settings.setup.deferredUse", settings::SETUP_DEFERRED_USE),
        (
            "settings.setup.deferredDismiss",
            settings::SETUP_DEFERRED_DISMISS,
        ),
        ("settings.home.sourcesNotice", settings::HOME_SOURCES_NOTICE),
        (
            "settings.home.sourcesNotice.dismiss",
            settings::HOME_SOURCES_NOTICE_DISMISS,
        ),
        (
            "settings.sources.browser.offer",
            settings::SOURCES_BROWSER_OFFER,
        ),
        (
            "settings.sources.browser.stores",
            settings::SOURCES_BROWSER_STORES,
        ),
        (
            "settings.sources.browser.storeError",
            settings::SOURCES_BROWSER_STORE_ERROR,
        ),
        ("settings.pairing.state", settings::PAIRING_STATE),
        ("settings.pairing.input", settings::PAIRING_INPUT),
        ("settings.pairing.submit", settings::PAIRING_SUBMIT),
        (
            "settings.pairing.finishConnecting",
            settings::PAIRING_FINISH_CONNECTING,
        ),
        (
            "settings.setup.browserJournal",
            settings::SETUP_BROWSER_JOURNAL,
        ),
        ("settings.pairing.mark", settings::PAIRING_MARK),
        ("settings.pairing.markCard", settings::PAIRING_MARK_CARD),
        ("settings.pairing.confirmYes", settings::PAIRING_CONFIRM_YES),
        (
            "settings.pairing.confirmReject",
            settings::PAIRING_CONFIRM_REJECT,
        ),
        (
            "settings.pairing.confirmContinue",
            settings::PAIRING_CONFIRM_CONTINUE,
        ),
        (
            "settings.pairing.confirmCancel",
            settings::PAIRING_CONFIRM_CANCEL,
        ),
        ("settings.journal.open", settings::JOURNAL_OPEN),
        (
            "settings.journal.unavailable",
            settings::JOURNAL_UNAVAILABLE,
        ),
        (
            "settings.unknown-journal.list",
            settings::UNKNOWN_JOURNAL_LIST,
        ),
        (
            "settings.unknown-journal.summary",
            settings::UNKNOWN_JOURNAL_SUMMARY,
        ),
        (
            "settings.unknown-journal.detail",
            settings::UNKNOWN_JOURNAL_DETAIL,
        ),
        (
            "settings.unknown-journal.what-answered",
            settings::UNKNOWN_JOURNAL_WHAT_ANSWERED,
        ),
        (
            "settings.unknown-journal.your-journal",
            settings::UNKNOWN_JOURNAL_YOUR_JOURNAL,
        ),
        (
            "settings.unknown-journal.responding-caption",
            settings::UNKNOWN_JOURNAL_RESPONDING_CAPTION,
        ),
        ("settings.updates.state", settings::UPDATES_STATE),
        (
            "settings.updates.lastChecked",
            settings::UPDATES_LAST_CHECKED,
        ),
        ("settings.updates.checkNow", settings::UPDATES_CHECK_NOW),
        ("settings.updates.cancel", settings::UPDATES_CANCEL),
        ("settings.updates.download", settings::UPDATES_DOWNLOAD),
        ("settings.updates.install", settings::UPDATES_INSTALL),
        ("settings.updates.retry", settings::UPDATES_RETRY),
        ("settings.updates.dismiss", settings::UPDATES_DISMISS),
        ("settings.updates.autoCheck", settings::UPDATES_AUTO_CHECK),
        ("settings.updates.frequency", settings::UPDATES_FREQUENCY),
        (
            "settings.updates.autoDownload",
            settings::UPDATES_AUTO_DOWNLOAD,
        ),
        ("settings.updates.notes", settings::UPDATES_NOTES),
        (
            "settings.exclusions.privateBrowsing",
            settings::EXCLUSIONS_PRIVATE_BROWSING,
        ),
        (
            "settings.exclusions.appInput",
            settings::EXCLUSIONS_APP_INPUT,
        ),
        ("settings.exclusions.appAdd", settings::EXCLUSIONS_APP_ADD),
        (
            "settings.exclusions.appsList",
            settings::EXCLUSIONS_APPS_LIST,
        ),
        (
            "settings.exclusions.titleInput",
            settings::EXCLUSIONS_TITLE_INPUT,
        ),
        (
            "settings.exclusions.titleAdd",
            settings::EXCLUSIONS_TITLE_ADD,
        ),
        (
            "settings.exclusions.titlesList",
            settings::EXCLUSIONS_TITLES_LIST,
        ),
        (
            "settings.exclusions.activity",
            settings::EXCLUSIONS_ACTIVITY,
        ),
        ("settings.hotkey.enabled", settings::HOTKEY_ENABLED),
        ("settings.hotkey.combo", settings::HOTKEY_COMBO),
        ("settings.hotkey.clear", settings::HOTKEY_CLEAR),
        ("settings.hotkey.status", settings::HOTKEY_STATUS),
        ("settings.mic.devices", settings::MIC_DEVICES),
        ("settings.mic.active", settings::MIC_ACTIVE),
        ("settings.mic.gain", settings::MIC_GAIN),
        ("about.window.root", about::WINDOW_ROOT),
        ("about.version", about::VERSION),
        ("about.journalVersion", about::JOURNAL_VERSION),
    ])
}

/// The state/source token vocabulary, *derived* from the model enums. Adding a
/// variant in `observer-model` changes this output and trips the drift gate.
fn state_tokens() -> BTreeMap<&'static str, Vec<String>> {
    BTreeMap::from([
        ("app_phase", enum_tokens::<AppPhase>()),
        ("source_kind", enum_tokens::<SourceKind>()),
        ("source_status", source_status_tokens()),
        ("pause_reason", enum_tokens::<PauseReason>()),
        ("error_reason", enum_tokens::<ErrorReason>()),
        ("pairing_phase", enum_tokens::<PairingPhase>()),
        ("view_render_state", enum_tokens::<ViewRenderState>()),
    ])
}

/// Serialized snake_case tokens for any `EnumIter + Into<&'static str>` enum,
/// sorted for determinism.
fn enum_tokens<E>() -> Vec<String>
where
    E: IntoEnumIterator + Into<&'static str>,
{
    let mut v: Vec<String> = E::iter()
        .map(|variant| {
            let s: &'static str = variant.into();
            s.to_string()
        })
        .collect();
    v.sort();
    v
}

/// `SourceState`'s serde `status` tags. `SourceState` carries data on `Faulted`
/// so it is not a `strum` unit enum; its tags are enumerated explicitly and the
/// `non_exhaustive_check` test guards against silent drift.
fn source_status_tokens() -> Vec<String> {
    let mut v = vec![
        "active".to_string(),
        "inactive".to_string(),
        "no_input_device".to_string(),
        "faulted".to_string(),
    ];
    v.sort();
    v
}

#[derive(Serialize)]
struct EndpointControl {
    host: &'static str,
    port: u16,
}

#[derive(Serialize)]
struct EndpointHealth {
    host: &'static str,
    path: &'static str,
    port: u16,
}

#[derive(Serialize)]
struct Endpoints {
    control: EndpointControl,
    health: EndpointHealth,
}

/// The whole contract document, in the exact key order it serializes (BTreeMap
/// + `_generated` sorting first under ASCII).
#[derive(Serialize)]
struct Contract {
    #[serde(rename = "_generated")]
    generated: &'static str,
    automation_ids: BTreeMap<&'static str, &'static str>,
    endpoints: Endpoints,
    state_tokens: BTreeMap<&'static str, Vec<String>>,
}

/// Render the canonical `automation-contract.json` text: deterministic
/// (sorted keys via BTreeMap + pretty), with a trailing newline.
pub fn generate_contract() -> String {
    let contract = Contract {
        generated: GENERATED_BANNER,
        automation_ids: automation_ids(),
        endpoints: Endpoints {
            control: EndpointControl {
                host: LOOPBACK_HOST,
                port: CONTROL_PORT,
            },
            health: EndpointHealth {
                host: LOOPBACK_HOST,
                path: HEALTH_PATH,
                port: HEALTH_PORT,
            },
        },
        state_tokens: state_tokens(),
    };
    let mut out = serde_json::to_string_pretty(&contract)
        .expect("contract serializes; it is plain owned data");
    out.push('\n');
    out
}

// ── Setup Handoff Contract Constants ──────────────────────────────────────────

pub const MAILBOX_SCHEMA: &str = "solstone.windows-setup-handoff.v1";
pub const PROGRESS_SCHEMA: &str = "solstone.setup-handoff-progress.v1";
pub const RECEIPT_SCHEMA: &str = "solstone.windows-setup-receipt.v1";
pub const DRAFT_SCHEMA: &str = "solstone.pair-link-draft.v1";
pub const CAPABILITY_SCHEMA: &str = "solstone.windows-setup-capabilities.v1";

pub const MAILBOX_FILE_NAME: &str = "solstone-setup-handoff.json";
pub const PROGRESS_FILE_NAME: &str = "setup-handoff-progress.json";
pub const RECEIPT_FILE_NAME: &str = "solstone-setup-handoff-receipt.json";
pub const LOCK_FILE_NAME: &str = "solstone-setup-handoff.lock";
pub const DRAFT_FILE_NAME: &str = "pair-link-draft.json";
pub const DEFERRED_FILE_NAME: &str = "pair-link-deferred.json";
pub const DEFERRED_REJECTED_FILE_NAME: &str = "pair-link-deferred-rejected";

pub const MAILBOX_MAX_BYTES: usize = 16384;
pub const RECEIPT_MAX_BYTES: usize = 4096;
pub const PROGRESS_MAX_BYTES: usize = 4096;
pub const DRAFT_MAX_BYTES: usize = 4096;
pub const LOCK_ACQUIRE_MS: u64 = 5000;
pub const CAPABILITY_RESOURCE_MAX_BYTES: usize = 4096;
pub const UNPAIRED_DRAFT_MAX_AGE_SECS: u64 = 900;

/// Literal install path the sender may launch. Scheme registration must not use it.
pub const INSTALLED_EXECUTABLE: &str = r"%LOCALAPPDATA%\Solstone\current\solstone-windows-app.exe";

/// Lowercase 8-4-4-4-12 hex with hyphens. Uppercase, missing hyphens, and braces fail.
pub fn is_canonical_uuid(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.len() != 36 {
        return false;
    }
    for (index, byte) in bytes.iter().enumerate() {
        let hyphen = index == 8 || index == 13 || index == 18 || index == 23;
        if hyphen {
            if *byte != b'-' {
                return false;
            }
        } else if !byte.is_ascii_hexdigit() || byte.is_ascii_uppercase() {
            return false;
        }
    }
    true
}

pub const IDENTITY_METHOD: &str = "GET";
pub const IDENTITY_PATH: &str = "/app/link/api/identity";

pub const CAPABILITY_RESOURCE_NAME: &str = "SOLSTONE_SETUP_CAPABILITIES";
pub const CAPABILITY_RESOURCE_TYPE: &str = "RT_RCDATA";
pub const CAPABILITY_JSON: &str =
    r#"{"schema":"solstone.windows-setup-capabilities.v1","capabilities":["setup-sequence-v1"]}"#;

pub const DESKTOP_SCHEME: &str = "solstone";
pub const DESKTOP_HOST: &str = "pair";
pub const DESKTOP_PATHS: &[&str] = &["", "/"];
pub const CANONICAL_PREFIX: &str = "https://go.solstone.app/p#";
pub const HANDOFF_REQUIRED_STEP: u32 = 5;

pub fn capability_resource_bytes() -> &'static [u8] {
    CAPABILITY_JSON.as_bytes()
}

/// Quoted RC include of the capability file. A quoted RC string is null-terminated,
/// so the resource is the file itself and not a `BEGIN` integer list.
pub fn capability_rc_fragment(path: &std::path::Path) -> String {
    // RC's reserved RCDATA keyword emits numeric RT_RCDATA (10). The API macro
    // name becomes a custom string type when the RC script has no Windows header.
    format!(
        "{} RCDATA {}\n",
        CAPABILITY_RESOURCE_NAME,
        rc_quoted_path(path)
    )
}

fn rc_quoted_path(path: &std::path::Path) -> String {
    let mut quoted = String::from("\"");
    for ch in path.to_string_lossy().chars() {
        match ch {
            '\\' => quoted.push_str("\\\\"),
            '"' => quoted.push_str("\"\""),
            _ => quoted.push(ch),
        }
    }
    quoted.push('"');
    quoted
}

#[derive(Serialize)]
struct SetupHandoffContract {
    #[serde(rename = "_generated")]
    generated: &'static str,
    argv: SetupHandoffArgv,
    capability_document: serde_json::Value,
    desktop: SetupHandoffDesktop,
    fields: SetupHandoffFields,
    files: SetupHandoffFiles,
    identity: SetupHandoffIdentity,
    limits: SetupHandoffLimits,
    resource: SetupHandoffResource,
    schemas: SetupHandoffSchemas,
}

#[derive(Serialize)]
struct SetupHandoffArgv {
    cli_flag: &'static str,
    control_verb: &'static str,
    installed_executable: &'static str,
}

#[derive(Serialize)]
struct SetupHandoffDesktop {
    canonical_prefix: &'static str,
    host: &'static str,
    paths: &'static [&'static str],
    scheme: &'static str,
}

#[derive(Serialize)]
struct SetupHandoffField {
    name: &'static str,
    #[serde(rename = "type")]
    type_name: &'static str,
    #[serde(rename = "const", skip_serializing_if = "Option::is_none")]
    const_value: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct SetupHandoffFieldSet {
    unknown_fields: &'static str,
    required: Vec<SetupHandoffField>,
}

#[derive(Serialize)]
struct SetupHandoffFields {
    mailbox: SetupHandoffFieldSet,
    receipt: SetupHandoffFieldSet,
}

#[derive(Serialize)]
struct SetupHandoffFiles {
    lock: &'static str,
    mailbox: &'static str,
    receipt: &'static str,
}

#[derive(Serialize)]
struct SetupHandoffIdentity {
    method: &'static str,
    path: &'static str,
}

#[derive(Serialize)]
struct SetupHandoffLimits {
    mailbox_max_bytes: usize,
    receipt_max_bytes: usize,
    capability_resource_max_bytes: usize,
    lock_acquire_ms: u64,
    unpaired_draft_max_age_secs: u64,
}

#[derive(Serialize)]
struct SetupHandoffResource {
    name: &'static str,
    r#type: &'static str,
}

#[derive(Serialize)]
struct SetupHandoffSchemas {
    mailbox: &'static str,
    receipt: &'static str,
}

fn required_field(
    name: &'static str,
    type_name: &'static str,
    const_value: Option<serde_json::Value>,
) -> SetupHandoffField {
    SetupHandoffField {
        name,
        type_name,
        const_value,
    }
}

pub fn generate_setup_handoff_contract() -> String {
    let contract = SetupHandoffContract {
        generated: GENERATED_BANNER,
        argv: SetupHandoffArgv {
            cli_flag: SETUP_HANDOFF_ARG,
            control_verb: std::str::from_utf8(RELOAD_SETUP_VERB)
                .expect("reload-setup verb is valid utf-8"),
            installed_executable: INSTALLED_EXECUTABLE,
        },
        capability_document: serde_json::from_str(CAPABILITY_JSON)
            .expect("capability JSON is valid"),
        desktop: SetupHandoffDesktop {
            canonical_prefix: CANONICAL_PREFIX,
            host: DESKTOP_HOST,
            paths: DESKTOP_PATHS,
            scheme: DESKTOP_SCHEME,
        },
        fields: SetupHandoffFields {
            mailbox: SetupHandoffFieldSet {
                unknown_fields: "forbidden",
                required: vec![
                    required_field("schema", "string", None),
                    required_field("handoff_id", "uuid", None),
                    required_field(
                        "step",
                        "integer",
                        Some(serde_json::json!(HANDOFF_REQUIRED_STEP)),
                    ),
                    required_field("expected_instance_id", "string", None),
                    required_field("expected_ca_fingerprint", "sha256-hex", None),
                    required_field("pairing_link", "https-pair-link", None),
                    required_field("expires_at_unix_ms", "unix-milliseconds", None),
                ],
            },
            receipt: SetupHandoffFieldSet {
                unknown_fields: "forbidden",
                required: vec![
                    required_field("schema", "string", None),
                    required_field("handoff_id", "uuid", None),
                    required_field("accepted", "boolean", Some(serde_json::json!(true))),
                ],
            },
        },
        files: SetupHandoffFiles {
            lock: LOCK_FILE_NAME,
            mailbox: MAILBOX_FILE_NAME,
            receipt: RECEIPT_FILE_NAME,
        },
        identity: SetupHandoffIdentity {
            method: IDENTITY_METHOD,
            path: IDENTITY_PATH,
        },
        limits: SetupHandoffLimits {
            mailbox_max_bytes: MAILBOX_MAX_BYTES,
            receipt_max_bytes: RECEIPT_MAX_BYTES,
            capability_resource_max_bytes: CAPABILITY_RESOURCE_MAX_BYTES,
            lock_acquire_ms: LOCK_ACQUIRE_MS,
            unpaired_draft_max_age_secs: UNPAIRED_DRAFT_MAX_AGE_SECS,
        },
        resource: SetupHandoffResource {
            name: CAPABILITY_RESOURCE_NAME,
            r#type: CAPABILITY_RESOURCE_TYPE,
        },
        schemas: SetupHandoffSchemas {
            mailbox: MAILBOX_SCHEMA,
            receipt: RECEIPT_SCHEMA,
        },
    };
    let mut out = serde_json::to_string_pretty(&contract)
        .expect("setup-handoff contract serializes; it is plain owned data");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::SourceState;
    use serde_json::Value;

    #[test]
    fn first_key_is_the_generated_banner() {
        let json = generate_contract();
        // The `_generated` key sorts first under ASCII ('_' < 'a' < 's').
        let first = json.lines().nth(1).unwrap();
        assert!(first.contains("\"_generated\""), "got: {first}");
        assert!(first.contains(GENERATED_BANNER));
    }

    #[test]
    fn ends_with_trailing_newline() {
        assert!(generate_contract().ends_with("}\n"));
    }

    #[test]
    fn output_is_deterministic() {
        assert_eq!(generate_contract(), generate_contract());
    }

    #[test]
    fn state_tokens_track_the_model_enums() {
        let json = generate_contract();
        // A few sentinel tokens that prove the derive ran.
        assert!(json.contains("\"observing\""));
        assert!(json.contains("\"no_input_device\""));
        assert!(json.contains("\"system_audio\""));
    }

    #[test]
    fn generated_endpoints_match_constants() {
        let json = generate_contract();
        let value: Value = serde_json::from_str(&json).unwrap();
        assert_eq!(
            value["endpoints"]["control"]["host"].as_str(),
            Some(LOOPBACK_HOST)
        );
        assert_eq!(
            value["endpoints"]["control"]["port"].as_u64(),
            Some(CONTROL_PORT as u64)
        );
        assert_eq!(
            value["endpoints"]["health"]["host"].as_str(),
            Some(LOOPBACK_HOST)
        );
        assert_eq!(
            value["endpoints"]["health"]["path"].as_str(),
            Some(HEALTH_PATH)
        );
        assert_eq!(
            value["endpoints"]["health"]["port"].as_u64(),
            Some(HEALTH_PORT as u64)
        );
        assert!(value["automation_ids"]["settings.window.root"].is_string());
        assert!(value["state_tokens"]["app_phase"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str() == Some("observing")));
    }

    /// If a `SourceState` variant is added/removed, this fails until
    /// `source_status_tokens` is updated — the explicit guard for the one
    /// data-carrying enum strum can't iterate.
    #[test]
    fn source_status_variants_are_accounted_for() {
        fn assert_total(s: &SourceState) {
            match s {
                SourceState::Active
                | SourceState::Inactive
                | SourceState::NoInputDevice
                | SourceState::Faulted { .. } => {}
            }
        }
        assert_total(&SourceState::Active);
        assert_eq!(source_status_tokens().len(), 4);
    }

    #[test]
    fn setup_handoff_contract_first_key_is_the_generated_banner() {
        let json = generate_setup_handoff_contract();
        let first = json.lines().nth(1).unwrap();
        assert!(first.contains("\"_generated\""), "got: {first}");
        assert!(first.contains(GENERATED_BANNER));
    }

    #[test]
    fn setup_handoff_contract_ends_with_trailing_newline() {
        assert!(generate_setup_handoff_contract().ends_with("}\n"));
    }

    #[test]
    fn setup_handoff_contract_output_is_deterministic() {
        assert_eq!(
            generate_setup_handoff_contract(),
            generate_setup_handoff_contract()
        );
    }

    #[test]
    fn setup_handoff_contract_matches_sender_literals() {
        let actual: serde_json::Value =
            serde_json::from_str(&generate_setup_handoff_contract()).unwrap();
        assert_eq!(
            actual["argv"]["installed_executable"],
            "%LOCALAPPDATA%\\Solstone\\current\\solstone-windows-app.exe"
        );
        assert_eq!(actual["argv"]["cli_flag"], "--setup-handoff");
        assert_eq!(actual["argv"]["control_verb"], "reload-setup\n");
        assert_eq!(actual["files"]["mailbox"], "solstone-setup-handoff.json");
        assert_eq!(
            actual["files"]["receipt"],
            "solstone-setup-handoff-receipt.json"
        );
        assert_eq!(actual["files"]["lock"], "solstone-setup-handoff.lock");
        assert_eq!(
            actual["schemas"]["mailbox"],
            "solstone.windows-setup-handoff.v1"
        );
        assert_eq!(
            actual["schemas"]["receipt"],
            "solstone.windows-setup-receipt.v1"
        );
        assert_eq!(actual["limits"]["mailbox_max_bytes"], 16384);
        assert_eq!(actual["limits"]["receipt_max_bytes"], 4096);
        assert!(actual["limits"].get("max_document_bytes").is_none());
        let text = generate_setup_handoff_contract();
        assert!(!text.contains("setup-handoff-progress"));
        assert!(!text.contains("pair-link-draft"));
        assert!(!text.contains("pair-link-deferred"));
        assert!(!text.contains("max_document_bytes"));
        let mailbox_names: Vec<&str> = actual["fields"]["mailbox"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            mailbox_names,
            [
                "schema",
                "handoff_id",
                "step",
                "expected_instance_id",
                "expected_ca_fingerprint",
                "pairing_link",
                "expires_at_unix_ms",
            ]
        );
        assert_eq!(actual["fields"]["mailbox"]["unknown_fields"], "forbidden");
        assert_eq!(actual["fields"]["mailbox"]["required"][2]["const"], 5);
        let receipt_names: Vec<&str> = actual["fields"]["receipt"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .map(|field| field["name"].as_str().unwrap())
            .collect();
        assert_eq!(receipt_names, ["schema", "handoff_id", "accepted"]);
        assert_eq!(actual["fields"]["receipt"]["required"][2]["const"], true);
        assert_eq!(actual["fields"]["receipt"]["unknown_fields"], "forbidden");
    }

    #[test]
    fn capability_resource_bytes_match_the_utf8_json_and_do_not_prove_pe_extraction() {
        // This does not prove PE extraction.
        assert_eq!(capability_resource_bytes(), CAPABILITY_JSON.as_bytes());
        let path = std::path::Path::new(r"C:\out\solstone-setup-capabilities.json");
        let frag = capability_rc_fragment(path);
        assert!(frag.contains("SOLSTONE_SETUP_CAPABILITIES"));
        assert_eq!(frag.split_whitespace().nth(1), Some("RCDATA"));
        assert!(frag.contains("solstone-setup-capabilities.json"));
        assert!(!frag.contains("0x"));
        assert!(!frag.contains("BEGIN"));
    }

    #[test]
    fn new_automation_ids_present_in_contract() {
        let json = generate_contract();
        assert!(json.contains("settings.setup.stepNumber"));
        assert!(json.contains("settings.setup.freshLink"));
        assert!(json.contains("settings.setup.linkRetry"));
        assert!(json.contains("settings.pairing.finishConnecting"));
        assert!(json.contains("settings.setup.browserJournal"));
        assert!(json.contains("settings.setup.nativeConnect"));
        assert!(json.contains("settings.setup.deferredUse"));
        assert!(json.contains("settings.setup.deferredDismiss"));
    }
}
