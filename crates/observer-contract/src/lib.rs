// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! The shared automation contract — code, not prose.
//!
//! Two vocabularies live here as the single source of truth:
//!
//!  1. **AutomationId identifiers** — the namespaced `data-automation-id` /
//!     UIA AutomationId strings the FlaUI harness and the webview both reference
//!     (`tray.menu.start`, `settings.status.appState.state`, …). Declared as
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

use observer_model::{AppPhase, ErrorReason, PairingPhase, PauseReason, SourceKind};
use serde::Serialize;
use strum::IntoEnumIterator;

/// Header value pinned as the first key of the generated artifact.
pub const GENERATED_BANNER: &str = "DO NOT EDIT — run make contract";

// ── AutomationId source of truth ─────────────────────────────────────────────
// Namespaced identifiers. Renaming any of these is a compile-time break for
// every consumer that imports the const, and a contract-diff for the harness.

/// Tray surface.
pub mod tray {
    pub const ROOT: &str = "tray.root";
    pub const MENU_START: &str = "tray.menu.start";
    pub const MENU_PAUSE: &str = "tray.menu.pause";
    pub const MENU_RESUME: &str = "tray.menu.resume";
    pub const MENU_OPEN_SETTINGS: &str = "tray.menu.openSettings";
    pub const MENU_ABOUT: &str = "tray.menu.about";
    pub const MENU_QUIT: &str = "tray.menu.quit";
}

/// Settings window.
pub mod settings {
    pub const WINDOW_ROOT: &str = "settings.window.root";
    pub const STATUS_APP_STATE: &str = "settings.status.appState.state";
    pub const STATUS_SEGMENT_DIR: &str = "settings.status.segmentDir";
    pub const STATUS_UPLOAD_STATE: &str = "settings.status.upload.state";
    pub const SOURCES_SCREEN_STATE: &str = "settings.sources.screen.state";
    pub const SOURCES_SYSTEM_AUDIO_STATE: &str = "settings.sources.systemAudio.state";
    pub const SOURCES_MIC_STATE: &str = "settings.sources.mic.state";
    /// Pairing pane (Wave 2): phase, paired journal, the pair-link field + action.
    pub const PAIRING_STATE: &str = "settings.pairing.state";
    pub const PAIRING_JOURNAL: &str = "settings.pairing.journal";
    pub const PAIRING_INPUT: &str = "settings.pairing.input";
    pub const PAIRING_SUBMIT: &str = "settings.pairing.submit";
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
}

/// About window.
pub mod about {
    pub const WINDOW_ROOT: &str = "about.window.root";
    pub const VERSION: &str = "about.version";
}

/// Every AutomationId const, paired with a stable contract key. The contract key
/// is the JSON property; the value is the AutomationId string consumers stamp.
fn automation_ids() -> BTreeMap<&'static str, &'static str> {
    BTreeMap::from([
        ("tray.root", tray::ROOT),
        ("tray.menu.start", tray::MENU_START),
        ("tray.menu.pause", tray::MENU_PAUSE),
        ("tray.menu.resume", tray::MENU_RESUME),
        ("tray.menu.openSettings", tray::MENU_OPEN_SETTINGS),
        ("tray.menu.about", tray::MENU_ABOUT),
        ("tray.menu.quit", tray::MENU_QUIT),
        ("settings.window.root", settings::WINDOW_ROOT),
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
            "settings.sources.systemAudio.state",
            settings::SOURCES_SYSTEM_AUDIO_STATE,
        ),
        ("settings.sources.mic.state", settings::SOURCES_MIC_STATE),
        ("settings.pairing.state", settings::PAIRING_STATE),
        ("settings.pairing.journal", settings::PAIRING_JOURNAL),
        ("settings.pairing.input", settings::PAIRING_INPUT),
        ("settings.pairing.submit", settings::PAIRING_SUBMIT),
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
        ("about.window.root", about::WINDOW_ROOT),
        ("about.version", about::VERSION),
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

/// The whole contract document, in the exact key order it serializes (BTreeMap
/// + `_generated` sorting first under ASCII).
#[derive(Serialize)]
struct Contract {
    #[serde(rename = "_generated")]
    generated: &'static str,
    automation_ids: BTreeMap<&'static str, &'static str>,
    state_tokens: BTreeMap<&'static str, Vec<String>>,
}

/// Render the canonical `automation-contract.json` text: deterministic
/// (sorted keys via BTreeMap + pretty), with a trailing newline.
pub fn generate_contract() -> String {
    let contract = Contract {
        generated: GENERATED_BANNER,
        automation_ids: automation_ids(),
        state_tokens: state_tokens(),
    };
    let mut out = serde_json::to_string_pretty(&contract)
        .expect("contract serializes; it is plain owned data");
    out.push('\n');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::SourceState;

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
}
