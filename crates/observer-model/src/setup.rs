// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable setup and source preferences (`setup.json`).
//!
//! Owns the classification of fresh vs legacy installs, the pure login
//! decision evaluation, and atomic load/store of `%LocalAppData%\Solstone\setup.json`.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::SourceKind;

/// Release activation is changed only after native owner qualification.
pub const SETUP_SEQUENCE_ENABLED: bool = true;

pub fn is_setup_ingress(arg: &str) -> bool {
    let arg = arg.trim();
    arg == crate::SETUP_HANDOFF_ARG
        || arg
            .get(..11)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("solstone://"))
}

pub const CURRENT_SETUP_VERSION: u32 = 1;
pub const SETUP_FILE_NAME: &str = "setup.json";

/// Wizard progress step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WizardStep {
    Mic,
    SystemAudio,
    Screen,
    Connection,
    Done,
}

/// Durable schema for `setup.json` (version 1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupConfig {
    pub version: u32,
    pub mic: bool,
    pub system_audio: bool,
    pub screen: bool,
    pub start_at_login: bool,
    pub wizard: WizardStep,
    pub notice_dismissed: bool,
    #[serde(default)]
    pub desktop_activity: bool,
    #[serde(default)]
    pub desktop_activity_window_titles: bool,
    #[serde(default)]
    pub desktop_activity_page_addresses: bool,
}

impl SetupConfig {
    pub fn fresh() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: false,
            wizard: WizardStep::Mic,
            notice_dismissed: true,
            desktop_activity: false,
            desktop_activity_window_titles: false,
            desktop_activity_page_addresses: false,
        }
    }

    pub fn legacy() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: true,
            system_audio: true,
            screen: true,
            start_at_login: true,
            wizard: WizardStep::Done,
            notice_dismissed: false,
            desktop_activity: false,
            desktop_activity_window_titles: false,
            desktop_activity_page_addresses: false,
        }
    }

    pub fn fail_closed() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: false,
            wizard: WizardStep::Done,
            notice_dismissed: true,
            desktop_activity: false,
            desktop_activity_window_titles: false,
            desktop_activity_page_addresses: false,
        }
    }

    pub fn to_snapshot(&self) -> SetupSnapshot {
        SetupSnapshot {
            sequence_enabled: SETUP_SEQUENCE_ENABLED,
            mic: self.mic,
            system_audio: self.system_audio,
            screen: self.screen,
            start_at_login: self.start_at_login,
            wizard: self.wizard,
            notice_dismissed: self.notice_dismissed,
            desktop_activity: self.desktop_activity,
            desktop_activity_window_titles: self.desktop_activity_window_titles,
            desktop_activity_page_addresses: self.desktop_activity_page_addresses,
            desktop_activity_window_titles_save_failed: false,
            desktop_activity_page_addresses_save_failed: false,
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            handoff_id: None,
            handoff_revision: None,
            handoff_expired: false,
            handoff_new_connection: false,
            handoff_deferred: false,
            mailbox_busy: false,
            handoff_rejected: false,
            native_handoff_rejected: false,
            handoff_persist_failed: false,
            has_deferred_link: false,
        }
    }
}

/// Marker recording an attempted source toggle that could not be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailedSourceWrite {
    pub kind: SourceKind,
    pub attempted: bool,
}

/// Honest setup snapshot folded into HealthDump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSnapshot {
    #[serde(default)]
    pub sequence_enabled: bool,
    pub mic: bool,
    pub system_audio: bool,
    pub screen: bool,
    pub start_at_login: bool,
    pub wizard: WizardStep,
    pub notice_dismissed: bool,
    #[serde(default)]
    pub desktop_activity: bool,
    #[serde(default)]
    pub desktop_activity_window_titles: bool,
    #[serde(default)]
    pub desktop_activity_page_addresses: bool,
    #[serde(default)]
    pub desktop_activity_window_titles_save_failed: bool,
    #[serde(default)]
    pub desktop_activity_page_addresses_save_failed: bool,
    #[serde(default)]
    pub failed_sources: Vec<FailedSourceWrite>,
    #[serde(default)]
    pub wizard_save_failed: bool,
    #[serde(default)]
    pub pending_wizard: Option<WizardStep>,
    #[serde(default)]
    pub notice_save_failed: bool,
    #[serde(default)]
    pub login_apply_failed: bool,
    #[serde(default)]
    pub handoff_id: Option<String>,
    #[serde(default)]
    pub handoff_revision: Option<u64>,
    #[serde(default)]
    pub handoff_expired: bool,
    #[serde(default)]
    pub handoff_new_connection: bool,
    #[serde(default)]
    pub handoff_deferred: bool,
    #[serde(default)]
    pub mailbox_busy: bool,
    #[serde(default)]
    pub handoff_rejected: bool,
    #[serde(default)]
    pub native_handoff_rejected: bool,
    #[serde(default)]
    pub handoff_persist_failed: bool,
    #[serde(default)]
    pub has_deferred_link: bool,
}

impl Default for SetupSnapshot {
    fn default() -> Self {
        Self {
            sequence_enabled: SETUP_SEQUENCE_ENABLED,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: false,
            wizard: WizardStep::Done,
            notice_dismissed: true,
            desktop_activity: false,
            desktop_activity_window_titles: false,
            desktop_activity_page_addresses: false,
            desktop_activity_window_titles_save_failed: false,
            desktop_activity_page_addresses_save_failed: false,
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            handoff_id: None,
            handoff_revision: None,
            handoff_expired: false,
            handoff_new_connection: false,
            handoff_deferred: false,
            mailbox_busy: false,
            handoff_rejected: false,
            native_handoff_rejected: false,
            handoff_persist_failed: false,
            has_deferred_link: false,
        }
    }
}

/// In-memory handoff facts. `set_handoff_overlay` is the only writer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HandoffOverlay {
    pub handoff_id: Option<String>,
    pub handoff_revision: Option<u64>,
    pub handoff_expired: bool,
    pub handoff_new_connection: bool,
    pub handoff_deferred: bool,
    pub mailbox_busy: bool,
    pub handoff_rejected: bool,
    pub native_handoff_rejected: bool,
    pub handoff_persist_failed: bool,
    pub has_deferred_link: bool,
}

impl SetupSnapshot {
    pub fn is_enabled(&self, kind: SourceKind) -> bool {
        match kind {
            SourceKind::Screen => self.screen,
            SourceKind::SystemAudio => self.system_audio,
            SourceKind::Mic => self.mic,
            SourceKind::DesktopActivity => self.desktop_activity,
        }
    }

    pub fn any_enabled(&self) -> bool {
        self.mic || self.system_audio || self.screen || self.desktop_activity
    }

    pub fn deliberate_off(&self) -> bool {
        !self.any_enabled()
    }
}

/// Outcome of classification and load on startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadOutcome {
    Fresh(SetupConfig),
    Legacy(SetupConfig),
    Valid(SetupConfig),
    FailedClosed(SetupConfig),
    Abort,
    LegacyUnsaved(SetupConfig),
}

const PREDECESSOR_FILES: &[&str] = &[
    "pairing.json",
    "mic.json",
    "exclusions.json",
    "hotkey.json",
    "update.json",
    "pause.txt",
];

/// Scan the data root for predecessor evidence of a prior run.
///
/// Predecessor evidence closed set:
/// - files: pairing.json, mic.json, exclusions.json, hotkey.json, update.json, pause.txt
/// - file: packages/.betaId
/// - any direct child inside segments/
///
/// Not evidence: the data root itself, anything under logs/, setup.json,
/// empty segments/, packages/ without .betaId. Unreadable stat on a candidate
/// counts as evidence (not fresh). NotFound does not.
pub fn has_predecessor_evidence(data_root: &Path) -> Result<bool, io::Error> {
    // If the data root cannot be listed (and is not NotFound), we cannot prove it is clean.
    match fs::read_dir(data_root) {
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    }

    // Check direct candidate files
    for filename in PREDECESSOR_FILES {
        let p = data_root.join(filename);
        match fs::symlink_metadata(&p) {
            Ok(_) => return Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(_) => return Ok(true), // Unreadable stat on candidate counts as evidence
        }
    }

    // Check packages/.betaId
    let beta_id = data_root.join("packages").join(".betaId");
    match fs::symlink_metadata(&beta_id) {
        Ok(_) => return Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Ok(true),
    }

    // Check segments/ children
    let segments_dir = data_root.join("segments");
    match fs::read_dir(&segments_dir) {
        Ok(mut entries) => {
            if entries.next().is_some() {
                return Ok(true);
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Ok(true), // Unreadable stat on candidate counts as evidence
    }

    Ok(false)
}

/// Atomically write `SetupConfig` to `setup.json`.
pub fn atomic_write_setup(data_root: &Path, config: &SetupConfig) -> io::Result<()> {
    if !data_root.exists() {
        fs::create_dir_all(data_root)?;
    }
    let target = data_root.join(SETUP_FILE_NAME);
    let tmp = data_root.join(format!("{SETUP_FILE_NAME}.tmp"));

    let json_bytes = serde_json::to_vec_pretty(config)
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

    let res = (|| -> io::Result<()> {
        let mut file = File::create(&tmp)?;
        file.write_all(&json_bytes)?;
        file.sync_all()?;
        fs::rename(&tmp, &target)?;
        Ok(())
    })();

    if res.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    res
}

/// Read `setup.json` read-only without classifying or writing.
/// Missing or bad -> quiet fail-closed default.
pub fn read_readonly(data_root: &Path) -> SetupSnapshot {
    let setup_path = data_root.join(SETUP_FILE_NAME);
    let Ok(bytes) = fs::read(&setup_path) else {
        return SetupSnapshot::default();
    };
    match serde_json::from_slice::<SetupConfig>(&bytes) {
        Ok(config) if config.version == CURRENT_SETUP_VERSION => config.to_snapshot(),
        _ => SetupSnapshot::default(),
    }
}

/// Dark builds preserve existing consent without creating or migrating setup state.
/// Only a genuinely absent file selects the published legacy source defaults.
pub fn load_for_startup(data_root: &Path, sequence_enabled: bool) -> LoadOutcome {
    if sequence_enabled {
        return load_or_classify(data_root);
    }
    match fs::read(data_root.join(SETUP_FILE_NAME)) {
        Ok(bytes) => match serde_json::from_slice::<SetupConfig>(&bytes) {
            Ok(config) if config.version == CURRENT_SETUP_VERSION => LoadOutcome::Valid(config),
            _ => LoadOutcome::FailedClosed(SetupConfig::fail_closed()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut legacy = SetupConfig::legacy();
            legacy.notice_dismissed = true;
            LoadOutcome::LegacyUnsaved(legacy)
        }
        Err(_) => LoadOutcome::FailedClosed(SetupConfig::fail_closed()),
    }
}

/// Load existing `setup.json` or classify and migrate.
pub fn load_or_classify(data_root: &Path) -> LoadOutcome {
    let setup_path = data_root.join(SETUP_FILE_NAME);
    match fs::read(&setup_path) {
        Ok(bytes) => {
            match serde_json::from_slice::<SetupConfig>(&bytes) {
                Ok(config) if config.version == CURRENT_SETUP_VERSION => LoadOutcome::Valid(config),
                _ => {
                    // Malformed, unknown version, or corrupt: leave byte-for-byte, fail closed in-memory.
                    LoadOutcome::FailedClosed(SetupConfig::fail_closed())
                }
            }
        }
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            // Unreadable: leave byte-for-byte, fail closed in-memory.
            LoadOutcome::FailedClosed(SetupConfig::fail_closed())
        }
        Err(_) => {
            // Missing file. Check predecessor evidence.
            match has_predecessor_evidence(data_root) {
                Err(_) => LoadOutcome::Abort,
                Ok(true) => {
                    // Legacy migration
                    let legacy = SetupConfig::legacy();
                    match atomic_write_setup(data_root, &legacy) {
                        Ok(()) => LoadOutcome::Legacy(legacy),
                        Err(_) => LoadOutcome::LegacyUnsaved(legacy),
                    }
                }
                Ok(false) => {
                    // Fresh install
                    let fresh = SetupConfig::fresh();
                    match atomic_write_setup(data_root, &fresh) {
                        Ok(()) => LoadOutcome::Fresh(fresh),
                        Err(_) => LoadOutcome::Abort,
                    }
                }
            }
        }
    }
}

/// Read-only inspection of `setup.json` for `not_running_snapshot`.
/// Never classifies and never writes.
pub fn read_setup_read_only(data_root: &Path) -> SetupSnapshot {
    let setup_path = data_root.join(SETUP_FILE_NAME);
    match fs::read(&setup_path) {
        Ok(bytes) => match serde_json::from_slice::<SetupConfig>(&bytes) {
            Ok(config) if config.version == CURRENT_SETUP_VERSION => config.to_snapshot(),
            _ => SetupSnapshot::default(),
        },
        Err(_) => SetupSnapshot::default(),
    }
}

/// Outcome of retrying unpersisted setup changes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetryOutcome {
    pub started: Vec<SourceKind>,
    pub saved: bool,
}

/// In-memory/durable coordinator for setup configuration and runtime source choices.
#[derive(Debug)]
pub struct SetupAuthority {
    root: Option<PathBuf>,
    durable: SetupConfig,
    running_mic: bool,
    running_system_audio: bool,
    running_screen: bool,
    running_desktop_activity: bool,
    failed_sources: Vec<FailedSourceWrite>,
    wizard_save_failed: bool,
    pending_wizard: Option<WizardStep>,
    notice_save_failed: bool,
    login_apply_failed: bool,
    pending_login: Option<bool>,
    pending_window_titles: Option<bool>,
    pending_page_addresses: Option<bool>,
    window_titles_save_failed: bool,
    page_addresses_save_failed: bool,
    overlay: HandoffOverlay,
}

impl Default for SetupAuthority {
    fn default() -> Self {
        Self::new()
    }
}

impl SetupAuthority {
    pub fn new() -> Self {
        Self {
            root: None,
            durable: SetupConfig::fresh(),
            running_mic: false,
            running_system_audio: false,
            running_screen: false,
            running_desktop_activity: false,
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            pending_login: None,
            pending_window_titles: None,
            pending_page_addresses: None,
            window_titles_save_failed: false,
            page_addresses_save_failed: false,
            overlay: HandoffOverlay::default(),
        }
    }

    pub fn set_root(&mut self, root: PathBuf) {
        self.root = Some(root);
    }

    pub fn set_handoff_overlay(&mut self, overlay: HandoffOverlay) {
        self.overlay = overlay;
    }

    pub fn seed(&mut self, snapshot: &SetupSnapshot) {
        self.durable.mic = snapshot.mic;
        self.durable.system_audio = snapshot.system_audio;
        self.durable.screen = snapshot.screen;
        self.durable.desktop_activity = snapshot.desktop_activity;
        self.durable.desktop_activity_window_titles = snapshot.desktop_activity_window_titles;
        self.durable.desktop_activity_page_addresses = snapshot.desktop_activity_page_addresses;
        self.running_mic = snapshot.mic;
        self.running_system_audio = snapshot.system_audio;
        self.running_screen = snapshot.screen;
        self.running_desktop_activity = snapshot.desktop_activity;
        self.durable.wizard = snapshot.wizard;
        self.durable.notice_dismissed = snapshot.notice_dismissed;
        self.durable.start_at_login = snapshot.start_at_login;
        self.login_apply_failed = snapshot.login_apply_failed;
        self.pending_login = None;
        self.pending_window_titles = None;
        self.pending_page_addresses = None;
        self.window_titles_save_failed = false;
        self.page_addresses_save_failed = false;

        self.failed_sources.clear();
        self.wizard_save_failed = false;
        self.pending_wizard = None;
        self.notice_save_failed = false;
    }

    pub fn projection(&self) -> SetupSnapshot {
        SetupSnapshot {
            sequence_enabled: SETUP_SEQUENCE_ENABLED,
            mic: self.running_mic,
            system_audio: self.running_system_audio,
            screen: self.running_screen,
            desktop_activity: self.running_desktop_activity,
            desktop_activity_window_titles: self.durable.desktop_activity_window_titles,
            desktop_activity_page_addresses: self.durable.desktop_activity_page_addresses,
            desktop_activity_window_titles_save_failed: self.window_titles_save_failed,
            desktop_activity_page_addresses_save_failed: self.page_addresses_save_failed,
            start_at_login: self.pending_login.unwrap_or(self.durable.start_at_login),
            wizard: self.durable.wizard,
            notice_dismissed: self.durable.notice_dismissed,
            failed_sources: self.failed_sources.clone(),
            wizard_save_failed: self.wizard_save_failed,
            pending_wizard: self.pending_wizard,
            notice_save_failed: self.notice_save_failed,
            login_apply_failed: self.login_apply_failed,
            handoff_id: self.overlay.handoff_id.clone(),
            handoff_revision: self.overlay.handoff_revision,
            handoff_expired: self.overlay.handoff_expired,
            handoff_new_connection: self.overlay.handoff_new_connection,
            handoff_deferred: self.overlay.handoff_deferred,
            mailbox_busy: self.overlay.mailbox_busy,
            handoff_rejected: self.overlay.handoff_rejected,
            native_handoff_rejected: self.overlay.native_handoff_rejected,
            handoff_persist_failed: self.overlay.handoff_persist_failed,
            has_deferred_link: self.overlay.has_deferred_link,
        }
    }

    // Any later successful save must carry an OFF choice that previously failed.
    // Failed ON requests remain inactive until their explicit retry.
    fn candidate_with_pending_off(&self) -> SetupConfig {
        let mut candidate = self.durable.clone();
        if self.pending_login == Some(false) {
            candidate.start_at_login = false;
        }
        if self.pending_window_titles == Some(false) {
            candidate.desktop_activity_window_titles = false;
        }
        if self.pending_page_addresses == Some(false) {
            candidate.desktop_activity_page_addresses = false;
        }
        for failed in self
            .failed_sources
            .iter()
            .filter(|failed| !failed.attempted)
        {
            match failed.kind {
                SourceKind::Screen => candidate.screen = false,
                SourceKind::SystemAudio => candidate.system_audio = false,
                SourceKind::Mic => candidate.mic = false,
                SourceKind::DesktopActivity => candidate.desktop_activity = false,
            }
        }
        candidate
    }

    fn persist(&mut self, candidate: SetupConfig) -> bool {
        if self
            .root
            .as_ref()
            .is_some_and(|root| atomic_write_setup(root, &candidate).is_err())
        {
            return false;
        }
        self.durable = candidate;
        self.failed_sources.retain(|failed| failed.attempted);
        true
    }

    pub fn set_source(&mut self, kind: SourceKind, enabled: bool) -> bool {
        if !enabled {
            self.set_running(kind, false);
        }
        let mut candidate = self.candidate_with_pending_off();
        match kind {
            SourceKind::Screen => candidate.screen = enabled,
            SourceKind::SystemAudio => candidate.system_audio = enabled,
            SourceKind::Mic => candidate.mic = enabled,
            SourceKind::DesktopActivity => candidate.desktop_activity = enabled,
        }
        if self.persist(candidate) {
            self.set_running(kind, enabled);
            self.remove_failed_source(kind);
            enabled
        } else {
            self.set_running(kind, false);
            self.record_failed_source(kind, enabled);
            false
        }
    }

    pub fn note_running(&mut self, kind: SourceKind, enabled: bool) {
        self.set_running(kind, enabled);
        if self.root.is_none() {
            match kind {
                SourceKind::Screen => self.durable.screen = enabled,
                SourceKind::SystemAudio => self.durable.system_audio = enabled,
                SourceKind::Mic => self.durable.mic = enabled,
                SourceKind::DesktopActivity => self.durable.desktop_activity = enabled,
            }
            self.remove_failed_source(kind);
        }
    }

    pub fn set_start_at_login(&mut self, enabled: bool) -> bool {
        self.pending_login = Some(enabled);
        let mut candidate = self.candidate_with_pending_off();
        candidate.start_at_login = enabled;
        self.persist(candidate)
    }

    pub fn set_login_apply_failed(&mut self, failed: bool) {
        self.login_apply_failed = failed;
        if !failed {
            self.pending_login = None;
        }
    }

    pub fn pending_login(&self) -> Option<bool> {
        self.pending_login.or_else(|| {
            self.login_apply_failed
                .then_some(self.durable.start_at_login)
        })
    }

    pub fn set_wizard_step(&mut self, step: WizardStep) -> bool {
        let mut candidate = self.candidate_with_pending_off();
        candidate.wizard = step;
        let saved = self.persist(candidate);
        self.wizard_save_failed = !saved;
        self.pending_wizard = if saved { None } else { Some(step) };
        saved
    }

    pub fn dismiss_notice(&mut self) -> bool {
        let mut candidate = self.candidate_with_pending_off();
        candidate.notice_dismissed = true;
        let saved = self.persist(candidate);
        self.notice_save_failed = !saved;
        saved
    }

    pub fn set_desktop_activity_window_titles(&mut self, enabled: bool) -> bool {
        let mut candidate = self.candidate_with_pending_off();
        candidate.desktop_activity_window_titles = enabled;
        if self.persist(candidate) {
            self.pending_window_titles = None;
            self.window_titles_save_failed = false;
            true
        } else {
            if !enabled {
                self.pending_window_titles = Some(false);
            } else {
                self.pending_window_titles = None;
            }
            self.window_titles_save_failed = true;
            false
        }
    }

    pub fn set_desktop_activity_page_addresses(&mut self, enabled: bool) -> bool {
        let mut candidate = self.candidate_with_pending_off();
        candidate.desktop_activity_page_addresses = enabled;
        if self.persist(candidate) {
            self.pending_page_addresses = None;
            self.page_addresses_save_failed = false;
            true
        } else {
            if !enabled {
                self.pending_page_addresses = Some(false);
            } else {
                self.pending_page_addresses = None;
            }
            self.page_addresses_save_failed = true;
            false
        }
    }

    pub fn set_desktop_activity_preferences(
        &mut self,
        window_titles: bool,
        page_addresses: bool,
    ) -> bool {
        let wt = self.set_desktop_activity_window_titles(window_titles);
        let pa = self.set_desktop_activity_page_addresses(page_addresses);
        wt && pa
    }

    pub fn set_preferences(&mut self, window_titles: bool, page_addresses: bool) -> bool {
        self.set_desktop_activity_preferences(window_titles, page_addresses)
    }

    pub fn retry(&mut self) -> RetryOutcome {
        let mut candidate = self.durable.clone();
        for fs in &self.failed_sources {
            match fs.kind {
                SourceKind::Screen => candidate.screen = fs.attempted,
                SourceKind::SystemAudio => candidate.system_audio = fs.attempted,
                SourceKind::Mic => candidate.mic = fs.attempted,
                SourceKind::DesktopActivity => candidate.desktop_activity = fs.attempted,
            }
        }
        if let Some(step) = self.pending_wizard {
            candidate.wizard = step;
        }
        if self.notice_save_failed {
            candidate.notice_dismissed = true;
        }
        if let Some(choice) = self.pending_login {
            candidate.start_at_login = choice;
        }
        if let Some(choice) = self.pending_window_titles {
            candidate.desktop_activity_window_titles = choice;
        }
        if let Some(choice) = self.pending_page_addresses {
            candidate.desktop_activity_page_addresses = choice;
        }

        let write_ok = if let Some(root) = &self.root {
            atomic_write_setup(root, &candidate).is_ok()
        } else {
            true
        };

        if write_ok {
            self.durable = candidate;
            self.pending_window_titles = None;
            self.pending_page_addresses = None;
            self.window_titles_save_failed = false;
            self.page_addresses_save_failed = false;
            let mut started = Vec::new();
            let failed = std::mem::take(&mut self.failed_sources);
            for fs in failed {
                if fs.attempted {
                    self.set_running(fs.kind, true);
                    started.push(fs.kind);
                } else {
                    self.set_running(fs.kind, false);
                }
            }
            self.wizard_save_failed = false;
            self.pending_wizard = None;
            self.notice_save_failed = false;
            RetryOutcome {
                started,
                saved: true,
            }
        } else {
            RetryOutcome {
                started: Vec::new(),
                saved: false,
            }
        }
    }

    fn set_running(&mut self, kind: SourceKind, enabled: bool) {
        match kind {
            SourceKind::Screen => self.running_screen = enabled,
            SourceKind::SystemAudio => self.running_system_audio = enabled,
            SourceKind::Mic => self.running_mic = enabled,
            SourceKind::DesktopActivity => self.running_desktop_activity = enabled,
        }
    }

    fn record_failed_source(&mut self, kind: SourceKind, attempted: bool) {
        if let Some(entry) = self.failed_sources.iter_mut().find(|f| f.kind == kind) {
            entry.attempted = attempted;
        } else {
            self.failed_sources
                .push(FailedSourceWrite { kind, attempted });
        }
    }

    fn remove_failed_source(&mut self, kind: SourceKind) {
        self.failed_sources.retain(|f| f.kind != kind);
    }
}

/// Executable installation type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyClass {
    Installed,
    Portable,
    Dev,
}

/// Observation of the current registry login item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RegistryReadState {
    Absent,
    Present(String),
    Unreadable,
}

/// Pure decision on what to do with the login item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoginDecision {
    Ensure,
    Remove,
    Leave,
    Failed,
}

/// Evaluate the pure login item decision.
pub fn evaluate_login_decision(
    copy_class: CopyClass,
    choice: Option<bool>,
    current: &RegistryReadState,
    my_command: &str,
    installed_owner_exists: Option<bool>,
) -> LoginDecision {
    let Some(choice) = choice else {
        return LoginDecision::Leave;
    };
    if matches!(copy_class, CopyClass::Dev) {
        return LoginDecision::Leave;
    }
    match current {
        RegistryReadState::Unreadable => LoginDecision::Failed,
        RegistryReadState::Absent | RegistryReadState::Present(_) => {
            if choice {
                match copy_class {
                    CopyClass::Installed => LoginDecision::Ensure,
                    CopyClass::Portable => match installed_owner_exists {
                        Some(true) => LoginDecision::Leave,
                        Some(false) => LoginDecision::Ensure,
                        None => LoginDecision::Failed,
                    },
                    CopyClass::Dev => LoginDecision::Leave,
                }
            } else {
                match current {
                    RegistryReadState::Present(cmd) => {
                        if cmd.eq_ignore_ascii_case(my_command) {
                            LoginDecision::Remove
                        } else {
                            LoginDecision::Leave
                        }
                    }
                    RegistryReadState::Absent => LoginDecision::Leave,
                    RegistryReadState::Unreadable => LoginDecision::Failed,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn temp_test_dir(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir()
            .join("solstone-setup-tests")
            .join(format!("{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn fresh_temp_dir_then_second_load_stays_all_off() {
        let root = temp_test_dir("fresh-twice");
        let first = load_or_classify(&root);
        assert_eq!(first, LoadOutcome::Fresh(SetupConfig::fresh()));

        let second = load_or_classify(&root);
        assert_eq!(second, LoadOutcome::Valid(SetupConfig::fresh()));

        // Creating logs/ or an empty segments/ maintains Valid fresh, all off
        fs::create_dir_all(root.join("logs")).unwrap();
        fs::create_dir_all(root.join("segments")).unwrap();

        let third = load_or_classify(&root);
        assert_eq!(third, LoadOutcome::Valid(SetupConfig::fresh()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn after_fresh_write_adding_evidence_keeps_valid_all_off() {
        let root = temp_test_dir("fresh-then-evidence");
        let first = load_or_classify(&root);
        assert_eq!(first, LoadOutcome::Fresh(SetupConfig::fresh()));

        // Add mic.json
        fs::write(root.join("mic.json"), b"{}").unwrap();

        // Valid setup.json is the authority!
        let second = load_or_classify(&root);
        assert_eq!(second, LoadOutcome::Valid(SetupConfig::fresh()));
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn corrupt_and_unknown_version_are_not_rewritten() {
        let root = temp_test_dir("corrupt");
        let bad_bytes = b"{\"version\": 99, \"mic\": true}";
        fs::write(root.join("setup.json"), bad_bytes).unwrap();

        let outcome = load_or_classify(&root);
        assert_eq!(
            outcome,
            LoadOutcome::FailedClosed(SetupConfig::fail_closed())
        );
        assert_eq!(fs::read(root.join("setup.json")).unwrap(), bad_bytes);

        let malformed = b"{not json}";
        fs::write(root.join("setup.json"), malformed).unwrap();
        let outcome2 = load_or_classify(&root);
        assert_eq!(
            outcome2,
            LoadOutcome::FailedClosed(SetupConfig::fail_closed())
        );
        assert_eq!(fs::read(root.join("setup.json")).unwrap(), malformed);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn subsequent_successful_save_persists_all_pending_off_choices() {
        let root = temp_test_dir("two-failed-offs");
        let initial = SetupConfig::legacy();
        atomic_write_setup(&root, &initial).unwrap();

        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&initial.to_snapshot());

        fs::create_dir(root.join("setup.json.tmp")).unwrap();

        let running_mic = auth.set_source(SourceKind::Mic, false);
        assert!(!running_mic);

        let running_screen = auth.set_source(SourceKind::Screen, false);
        assert!(!running_screen);

        let proj = auth.projection();
        assert!(!proj.mic);
        assert!(!proj.screen);
        assert_eq!(proj.failed_sources.len(), 2);
        assert_eq!(
            proj.failed_sources,
            vec![
                FailedSourceWrite {
                    kind: SourceKind::Mic,
                    attempted: false
                },
                FailedSourceWrite {
                    kind: SourceKind::Screen,
                    attempted: false
                }
            ]
        );

        let wizard_ok = auth.set_wizard_step(WizardStep::SystemAudio);
        assert!(!wizard_ok);
        let proj = auth.projection();
        assert_eq!(proj.wizard, WizardStep::Done);
        assert!(proj.wizard_save_failed);
        assert_eq!(proj.pending_wizard, Some(WizardStep::SystemAudio));

        let notice_ok = auth.dismiss_notice();
        assert!(!notice_ok);
        let proj = auth.projection();
        assert!(proj.notice_save_failed);

        fs::remove_dir(root.join("setup.json.tmp")).unwrap();

        let login_ok = auth.set_start_at_login(false);
        assert!(login_ok);
        let proj = auth.projection();
        assert!(proj.failed_sources.is_empty());

        let loaded = load_or_classify(&root);
        match loaded {
            LoadOutcome::Valid(cfg) => {
                assert!(!cfg.mic);
                assert!(!cfg.screen);
                assert!(!cfg.start_at_login);
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let outcome = auth.retry();
        assert!(outcome.started.is_empty());
        let proj = auth.projection();
        assert!(!proj.mic);
        assert!(!proj.screen);
        assert!(proj.failed_sources.is_empty());
        assert!(!proj.wizard_save_failed);
        assert!(!proj.notice_save_failed);
        assert_eq!(proj.wizard, WizardStep::SystemAudio);

        let loaded2 = load_or_classify(&root);
        match loaded2 {
            LoadOutcome::Valid(cfg) => {
                assert!(!cfg.mic);
                assert!(!cfg.screen);
                assert_eq!(cfg.wizard, WizardStep::SystemAudio);
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_on_never_projects_enabled() {
        let root = temp_test_dir("failed-on");
        let initial = SetupConfig::fresh();
        atomic_write_setup(&root, &initial).unwrap();

        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&initial.to_snapshot());

        fs::create_dir(root.join("setup.json.tmp")).unwrap();

        let running = auth.set_source(SourceKind::Mic, true);
        assert!(!running);

        let proj = auth.projection();
        assert!(!proj.mic);
        assert_eq!(
            proj.failed_sources,
            vec![FailedSourceWrite {
                kind: SourceKind::Mic,
                attempted: true
            }]
        );

        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_login_choice_survives_until_save_and_registration_succeed() {
        let root = temp_test_dir("login-retry");
        let initial = SetupConfig::fresh();
        atomic_write_setup(&root, &initial).unwrap();
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&initial.to_snapshot());
        fs::create_dir(root.join("setup.json.tmp")).unwrap();
        assert!(!auth.set_start_at_login(true));
        auth.set_login_apply_failed(true);
        assert!(auth.projection().start_at_login);
        assert_eq!(auth.pending_login(), Some(true));
        assert!(!auth.retry().saved);
        assert!(auth.projection().login_apply_failed);
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        assert!(auth.retry().saved);
        assert!(read_readonly(&root).start_at_login);
        // Saving is not registration success. Keep the intent for another retry.
        auth.set_login_apply_failed(true);
        assert_eq!(auth.pending_login(), Some(true));
        assert!(auth.retry().saved);
        auth.set_login_apply_failed(false);
        assert_eq!(auth.pending_login(), None);
        assert!(!auth.projection().login_apply_failed);
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn unrelated_save_preserves_failed_login_off_but_never_enables_failed_on() {
        for requested in [false, true] {
            let root = temp_test_dir("login-off-monotonic");
            let mut initial = SetupConfig::fresh();
            initial.start_at_login = !requested;
            atomic_write_setup(&root, &initial).unwrap();
            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&initial.to_snapshot());
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            assert!(!auth.set_start_at_login(requested));
            auth.set_login_apply_failed(true);
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            assert!(auth.set_wizard_step(WizardStep::Mic));
            assert!(!read_readonly(&root).start_at_login);
            assert_eq!(auth.pending_login(), Some(requested));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn login_decision_table() {
        let my_cmd = r#""C:\app.exe" --from-autostart"#;
        let other_cmd = r#""C:\other.exe" --from-autostart"#;

        // bad prefs (None) -> Leave
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                None,
                &RegistryReadState::Absent,
                my_cmd,
                None
            ),
            LoginDecision::Leave
        );

        // Dev -> always Leave
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Dev,
                Some(true),
                &RegistryReadState::Absent,
                my_cmd,
                None
            ),
            LoginDecision::Leave
        );
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Dev,
                Some(false),
                &RegistryReadState::Present(my_cmd.into()),
                my_cmd,
                None
            ),
            LoginDecision::Leave
        );

        // installed + on + readable -> Ensure
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(true),
                &RegistryReadState::Absent,
                my_cmd,
                None
            ),
            LoginDecision::Ensure
        );
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(true),
                &RegistryReadState::Present(other_cmd.into()),
                my_cmd,
                None
            ),
            LoginDecision::Ensure
        );

        // installed + on + unreadable -> Failed
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(true),
                &RegistryReadState::Unreadable,
                my_cmd,
                None
            ),
            LoginDecision::Failed
        );

        // portable + on + installed owner exists -> Leave
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Portable,
                Some(true),
                &RegistryReadState::Absent,
                my_cmd,
                Some(true)
            ),
            LoginDecision::Leave
        );

        // portable + on + no installed owner -> Ensure
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Portable,
                Some(true),
                &RegistryReadState::Absent,
                my_cmd,
                Some(false)
            ),
            LoginDecision::Ensure
        );

        // portable + on + unknown owner / unreadable -> Failed
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Portable,
                Some(true),
                &RegistryReadState::Absent,
                my_cmd,
                None
            ),
            LoginDecision::Failed
        );
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Portable,
                Some(true),
                &RegistryReadState::Unreadable,
                my_cmd,
                Some(false)
            ),
            LoginDecision::Failed
        );

        // off + installed/portable -> Remove only when present command equals my_command
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(false),
                &RegistryReadState::Present(my_cmd.into()),
                my_cmd,
                None
            ),
            LoginDecision::Remove
        );
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Portable,
                Some(false),
                &RegistryReadState::Present(my_cmd.into()),
                my_cmd,
                None
            ),
            LoginDecision::Remove
        );
        // ASCII case-insensitive
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(false),
                &RegistryReadState::Present(my_cmd.to_ascii_uppercase()),
                my_cmd,
                None
            ),
            LoginDecision::Remove
        );
        // different command -> Leave
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(false),
                &RegistryReadState::Present(other_cmd.into()),
                my_cmd,
                None
            ),
            LoginDecision::Leave
        );
        // absent -> Leave
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(false),
                &RegistryReadState::Absent,
                my_cmd,
                None
            ),
            LoginDecision::Leave
        );
        // unreadable -> Failed
        assert_eq!(
            evaluate_login_decision(
                CopyClass::Installed,
                Some(false),
                &RegistryReadState::Unreadable,
                my_cmd,
                None
            ),
            LoginDecision::Failed
        );
    }

    #[test]
    fn handoff_overlay_persists_across_wizard_step_without_modifying_sources() {
        let mut authority = SetupAuthority::new();
        authority.note_running(SourceKind::Mic, true);
        authority.note_running(SourceKind::SystemAudio, false);
        authority.note_running(SourceKind::Screen, true);

        authority.set_handoff_overlay(HandoffOverlay {
            handoff_id: Some("11111111-1111-4111-8111-111111111111".to_string()),
            handoff_revision: Some(42),
            handoff_expired: false,
            handoff_new_connection: true,
            handoff_deferred: true,
            mailbox_busy: true,
            handoff_rejected: false,
            native_handoff_rejected: false,
            handoff_persist_failed: false,
            has_deferred_link: true,
        });
        let before = authority.projection();
        assert_eq!(
            before.handoff_id.as_deref(),
            Some("11111111-1111-4111-8111-111111111111")
        );
        assert_eq!(before.handoff_revision, Some(42));
        assert!(before.handoff_new_connection);
        assert!(before.mailbox_busy);
        assert!(before.handoff_deferred);
        assert!(before.has_deferred_link);
        assert!(before.mic);
        assert!(!before.system_audio);
        assert!(before.screen);
        assert_eq!(before.wizard, WizardStep::Mic);

        authority.set_wizard_step(WizardStep::Connection);

        let after = authority.projection();
        assert_eq!(after.wizard, WizardStep::Connection);
        assert_eq!(after.handoff_id, before.handoff_id);
        assert_eq!(after.handoff_revision, before.handoff_revision);
        assert_eq!(after.has_deferred_link, before.has_deferred_link);
        assert!(after.mailbox_busy);
        assert!(after.handoff_deferred);
        assert_eq!(after.mic, before.mic);
        assert_eq!(after.system_audio, before.system_audio);
        assert_eq!(after.screen, before.screen);
    }

    #[test]
    fn failed_desktop_activity_preferences_keep_durable_value_and_set_failure_flags() {
        let root = temp_test_dir("failed-da-prefs");
        let mut initial = SetupConfig::fresh();
        initial.desktop_activity = true;
        initial.desktop_activity_window_titles = true;
        initial.desktop_activity_page_addresses = true;
        atomic_write_setup(&root, &initial).unwrap();

        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&initial.to_snapshot());

        // Block writing to setup.json
        fs::create_dir(root.join("setup.json.tmp")).unwrap();

        // Turning window_titles OFF fails persist
        assert!(!auth.set_desktop_activity_window_titles(false));
        let proj = auth.projection();
        assert!(proj.desktop_activity_window_titles);
        assert!(proj.desktop_activity_window_titles_save_failed);

        // Turning page_addresses OFF fails persist
        assert!(!auth.set_desktop_activity_page_addresses(false));
        let proj = auth.projection();
        assert!(proj.desktop_activity_page_addresses);
        assert!(proj.desktop_activity_page_addresses_save_failed);

        // Remove blocker and retry
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        let outcome = auth.retry();
        assert!(outcome.saved);

        let proj = auth.projection();
        assert!(!proj.desktop_activity_window_titles);
        assert!(!proj.desktop_activity_page_addresses);
        assert!(!proj.desktop_activity_window_titles_save_failed);
        assert!(!proj.desktop_activity_page_addresses_save_failed);

        match load_or_classify(&root) {
            LoadOutcome::Valid(cfg) => {
                assert!(!cfg.desktop_activity_window_titles);
                assert!(!cfg.desktop_activity_page_addresses);
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&root);
    }
}

#[cfg(test)]
mod activation_controls {
    #[test]
    fn setup_ingress_uses_the_dispatch_whitespace_rules() {
        for arg in [
            "--setup-handoff",
            " --setup-handoff ",
            "solstone://pair#x",
            " \tSOLSTONE://pair#x \r\n",
        ] {
            assert!(super::is_setup_ingress(arg));
        }
        for arg in [
            "--open-view",
            "--setup-handoff-other",
            "https://go.solstone.app/p#x",
        ] {
            assert!(!super::is_setup_ingress(arg));
        }
    }
}
