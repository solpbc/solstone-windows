// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable setup and source preferences (`setup.json`).
//!
//! Owns first-run classification, the pure login decision evaluation, and
//! atomic load/store of `%LocalAppData%\Solstone\setup.json`.

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

/// Version 2: desktop activity follows screen, and its settings are saved only
/// once the owner sets them.
pub const CURRENT_SETUP_VERSION: u32 = 2;
/// Version 1 wrote the desktop activity settings as first-run defaults, so a
/// saved value there is not an owner choice. Loading it discards those values
/// once and saves version 2.
const PRE_ACTIVITY_FOLLOWS_SCREEN_VERSION: u32 = 1;
pub const SETUP_FILE_NAME: &str = "setup.json";

/// Code defaults for the desktop activity settings. An unset setting is never
/// saved and reads as its default here, so a later default change reaches
/// every owner who has not chosen.
pub const DESKTOP_ACTIVITY_DEFAULT: bool = true;
pub const DESKTOP_ACTIVITY_THROUGH_PAUSE_DEFAULT: bool = false;
pub const DESKTOP_ACTIVITY_WINDOW_TITLES_DEFAULT: bool = true;
pub const DESKTOP_ACTIVITY_PAGE_ADDRESSES_DEFAULT: bool = true;

/// Whether desktop activity runs as a source: the owner has not turned it off,
/// and screen is on or the owner keeps activity on through a pause. The main
/// pause itself is applied by the engine.
pub fn desktop_activity_runs(allowed: bool, screen: bool, through_pause: bool) -> bool {
    allowed && (screen || through_pause)
}

/// The owner's saved desktop activity choices. `None` means not chosen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopActivityChoices {
    pub allowed: Option<bool>,
    pub through_pause: Option<bool>,
    pub window_titles: Option<bool>,
    pub page_addresses: Option<bool>,
}

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

/// Preference for starting the application automatically at user login.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LoginPreference {
    choice: Option<bool>,
}

impl LoginPreference {
    pub fn explicit(choice: bool) -> Self {
        Self {
            choice: Some(choice),
        }
    }

    pub fn unknown() -> Self {
        Self { choice: None }
    }

    pub fn choice(self) -> Option<bool> {
        self.choice
    }
}

impl Serialize for LoginPreference {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.choice.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for LoginPreference {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct LoginPreferenceVisitor;

        impl<'de> serde::de::Visitor<'de> for LoginPreferenceVisitor {
            type Value = LoginPreference;

            fn expecting(&self, formatter: &mut std::fmt::Formatter) -> std::fmt::Result {
                formatter.write_str("a boolean or null")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(LoginPreference::explicit(v))
            }

            fn visit_unit<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(LoginPreference::unknown())
            }

            fn visit_none<E>(self) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(LoginPreference::unknown())
            }

            fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                deserializer.deserialize_any(self)
            }
        }

        // serde 1.0.228 MissingFieldDeserializer::deserialize_option visits None,
        // so an omitted start_at_login would look unknown. deserialize_any rejects it.
        deserializer.deserialize_any(LoginPreferenceVisitor)
    }
}

/// Durable schema for `setup.json` (version 2).
///
/// The desktop activity settings hold only what the owner set: an unset one is
/// left out of the file and reads as its `DESKTOP_ACTIVITY_*_DEFAULT`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupConfig {
    pub version: u32,
    pub mic: bool,
    pub system_audio: bool,
    pub screen: bool,
    pub start_at_login: LoginPreference,
    pub wizard: WizardStep,
    pub notice_dismissed: bool,
    /// The owner's desktop activity switch. Off means activity never runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_activity: Option<bool>,
    /// Keep desktop activity on through the main pause and with screen off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_activity_through_pause: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_activity_window_titles: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub desktop_activity_page_addresses: Option<bool>,
}

impl SetupConfig {
    pub fn fresh() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: LoginPreference::explicit(false),
            wizard: WizardStep::Mic,
            notice_dismissed: true,
            desktop_activity: None,
            desktop_activity_through_pause: None,
            desktop_activity_window_titles: None,
            desktop_activity_page_addresses: None,
        }
    }

    pub fn legacy() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: true,
            system_audio: true,
            screen: true,
            start_at_login: LoginPreference::explicit(true),
            wizard: WizardStep::Done,
            notice_dismissed: false,
            desktop_activity: None,
            desktop_activity_through_pause: None,
            desktop_activity_window_titles: None,
            desktop_activity_page_addresses: None,
        }
    }

    pub fn fail_closed() -> Self {
        Self {
            version: CURRENT_SETUP_VERSION,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: LoginPreference::unknown(),
            wizard: WizardStep::Done,
            notice_dismissed: true,
            desktop_activity: None,
            desktop_activity_through_pause: None,
            desktop_activity_window_titles: None,
            desktop_activity_page_addresses: None,
        }
    }

    pub fn desktop_activity_choices(&self) -> DesktopActivityChoices {
        DesktopActivityChoices {
            allowed: self.desktop_activity,
            through_pause: self.desktop_activity_through_pause,
            window_titles: self.desktop_activity_window_titles,
            page_addresses: self.desktop_activity_page_addresses,
        }
    }

    fn set_desktop_activity_choices(&mut self, choices: DesktopActivityChoices) {
        self.desktop_activity = choices.allowed;
        self.desktop_activity_through_pause = choices.through_pause;
        self.desktop_activity_window_titles = choices.window_titles;
        self.desktop_activity_page_addresses = choices.page_addresses;
    }

    pub fn desktop_activity_allowed(&self) -> bool {
        self.desktop_activity.unwrap_or(DESKTOP_ACTIVITY_DEFAULT)
    }

    pub fn desktop_activity_through_pause(&self) -> bool {
        self.desktop_activity_through_pause
            .unwrap_or(DESKTOP_ACTIVITY_THROUGH_PAUSE_DEFAULT)
    }

    pub fn desktop_activity_window_titles(&self) -> bool {
        self.desktop_activity_window_titles
            .unwrap_or(DESKTOP_ACTIVITY_WINDOW_TITLES_DEFAULT)
    }

    pub fn desktop_activity_page_addresses(&self) -> bool {
        self.desktop_activity_page_addresses
            .unwrap_or(DESKTOP_ACTIVITY_PAGE_ADDRESSES_DEFAULT)
    }

    /// Whether desktop activity runs as a source under these choices.
    pub fn desktop_activity_enabled(&self) -> bool {
        desktop_activity_runs(
            self.desktop_activity_allowed(),
            self.screen,
            self.desktop_activity_through_pause(),
        )
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
            desktop_activity: self.desktop_activity_enabled(),
            desktop_activity_allowed: self.desktop_activity_allowed(),
            desktop_activity_through_pause: self.desktop_activity_through_pause(),
            desktop_activity_window_titles: self.desktop_activity_window_titles(),
            desktop_activity_page_addresses: self.desktop_activity_page_addresses(),
            desktop_activity_through_pause_save_failed: false,
            desktop_activity_window_titles_save_failed: false,
            desktop_activity_page_addresses_save_failed: false,
            desktop_activity_choices: self.desktop_activity_choices(),
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            setup_read_failed: false,
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
    pub start_at_login: LoginPreference,
    pub wizard: WizardStep,
    pub notice_dismissed: bool,
    /// Whether desktop activity runs as a source (see [`desktop_activity_runs`]).
    #[serde(default)]
    pub desktop_activity: bool,
    /// The owner's desktop activity switch, default on.
    #[serde(default = "default_desktop_activity_allowed")]
    pub desktop_activity_allowed: bool,
    #[serde(default)]
    pub desktop_activity_through_pause: bool,
    #[serde(default = "default_desktop_activity_window_titles")]
    pub desktop_activity_window_titles: bool,
    #[serde(default = "default_desktop_activity_page_addresses")]
    pub desktop_activity_page_addresses: bool,
    #[serde(default)]
    pub desktop_activity_through_pause_save_failed: bool,
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
    pub setup_read_failed: bool,
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
    /// The saved choices behind the desktop activity values above. Seeding
    /// from this keeps an unset choice unset, so a later save never writes a
    /// default.
    #[serde(skip)]
    pub desktop_activity_choices: DesktopActivityChoices,
}

fn default_desktop_activity_allowed() -> bool {
    DESKTOP_ACTIVITY_DEFAULT
}

fn default_desktop_activity_window_titles() -> bool {
    DESKTOP_ACTIVITY_WINDOW_TITLES_DEFAULT
}

fn default_desktop_activity_page_addresses() -> bool {
    DESKTOP_ACTIVITY_PAGE_ADDRESSES_DEFAULT
}

impl Default for SetupSnapshot {
    fn default() -> Self {
        Self {
            sequence_enabled: SETUP_SEQUENCE_ENABLED,
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: LoginPreference::explicit(false),
            wizard: WizardStep::Done,
            notice_dismissed: true,
            desktop_activity: false,
            desktop_activity_allowed: DESKTOP_ACTIVITY_DEFAULT,
            desktop_activity_through_pause: DESKTOP_ACTIVITY_THROUGH_PAUSE_DEFAULT,
            desktop_activity_window_titles: DESKTOP_ACTIVITY_WINDOW_TITLES_DEFAULT,
            desktop_activity_page_addresses: DESKTOP_ACTIVITY_PAGE_ADDRESSES_DEFAULT,
            desktop_activity_through_pause_save_failed: false,
            desktop_activity_window_titles_save_failed: false,
            desktop_activity_page_addresses_save_failed: false,
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            setup_read_failed: false,
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
            desktop_activity_choices: DesktopActivityChoices::default(),
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
    Valid(SetupConfig),
    FailedClosed(SetupConfig),
    Abort,
    LegacyUnsaved(SetupConfig),
}

/// Saved setup bytes read as the current version.
struct ParsedSetup {
    config: SetupConfig,
    /// The bytes were an older version; `config` is already upgraded.
    upgraded: bool,
}

/// Parse saved setup bytes. A version 1 file is upgraded in memory: its saved
/// desktop activity values were first-run defaults, not owner choices, so they
/// are discarded and read as the current defaults. Nothing else changes. Any
/// other version is unreadable.
fn parse_setup(bytes: &[u8]) -> Option<ParsedSetup> {
    let mut config = serde_json::from_slice::<SetupConfig>(bytes).ok()?;
    match config.version {
        CURRENT_SETUP_VERSION => Some(ParsedSetup {
            config,
            upgraded: false,
        }),
        PRE_ACTIVITY_FOLLOWS_SCREEN_VERSION => {
            config.version = CURRENT_SETUP_VERSION;
            config.set_desktop_activity_choices(DesktopActivityChoices::default());
            Some(ParsedSetup {
                config,
                upgraded: true,
            })
        }
        _ => None,
    }
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

fn read_setup_snapshot(data_root: &Path) -> SetupSnapshot {
    let setup_path = data_root.join(SETUP_FILE_NAME);
    let bytes = match fs::read(&setup_path) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return SetupSnapshot::default(),
        Err(_) => {
            let mut snapshot = SetupConfig::fail_closed().to_snapshot();
            snapshot.setup_read_failed = true;
            return snapshot;
        }
    };
    match parse_setup(&bytes) {
        Some(saved) => saved.config.to_snapshot(),
        None => {
            let mut snapshot = SetupConfig::fail_closed().to_snapshot();
            snapshot.setup_read_failed = true;
            snapshot
        }
    }
}

/// Read `setup.json` read-only without classifying or writing.
/// A missing file stays the quiet default. Damaged bytes report unknown login.
pub fn read_readonly(data_root: &Path) -> SetupSnapshot {
    read_setup_snapshot(data_root)
}

/// Dark builds preserve existing consent without creating or migrating setup state.
/// Only a genuinely absent file selects the published legacy source defaults.
pub fn load_for_startup(data_root: &Path, sequence_enabled: bool) -> LoadOutcome {
    if sequence_enabled {
        return load_or_classify(data_root);
    }
    match fs::read(data_root.join(SETUP_FILE_NAME)) {
        Ok(bytes) => match parse_setup(&bytes) {
            Some(saved) => LoadOutcome::Valid(saved.config),
            None => LoadOutcome::FailedClosed(SetupConfig::fail_closed()),
        },
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let mut legacy = SetupConfig::legacy();
            legacy.notice_dismissed = true;
            LoadOutcome::LegacyUnsaved(legacy)
        }
        Err(_) => LoadOutcome::FailedClosed(SetupConfig::fail_closed()),
    }
}

pub struct StartupSetup {
    pub config: SetupConfig,
    pub login_choice: Option<bool>,
    pub setup_read_failed: bool,
}

pub enum StartupLoad {
    Abort,
    Ready(StartupSetup),
}

pub fn startup_load(outcome: LoadOutcome) -> StartupLoad {
    match outcome {
        LoadOutcome::Abort => StartupLoad::Abort,
        LoadOutcome::Fresh(cfg) => StartupLoad::Ready(StartupSetup {
            login_choice: cfg.start_at_login.choice(),
            setup_read_failed: false,
            config: cfg,
        }),
        LoadOutcome::LegacyUnsaved(cfg) => StartupLoad::Ready(StartupSetup {
            login_choice: cfg.start_at_login.choice(),
            setup_read_failed: false,
            config: cfg,
        }),
        LoadOutcome::Valid(cfg) => StartupLoad::Ready(StartupSetup {
            login_choice: cfg.start_at_login.choice(),
            setup_read_failed: false,
            config: cfg,
        }),
        LoadOutcome::FailedClosed(_) => StartupLoad::Ready(StartupSetup {
            config: SetupConfig::fail_closed(),
            login_choice: None,
            setup_read_failed: true,
        }),
    }
}

/// Load existing `setup.json` or classify and migrate.
pub fn load_or_classify(data_root: &Path) -> LoadOutcome {
    let setup_path = data_root.join(SETUP_FILE_NAME);
    match fs::read(&setup_path) {
        Ok(bytes) => {
            match parse_setup(&bytes) {
                Some(saved) => {
                    if saved.upgraded {
                        // Save the upgrade once. If it cannot be saved, the
                        // next launch upgrades the same older bytes again.
                        let _ = atomic_write_setup(data_root, &saved.config);
                    }
                    LoadOutcome::Valid(saved.config)
                }
                None => {
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
            // Missing file: setup runs with every source off, whatever else is
            // on disk. Other files never stand in for the owner's choices.
            let fresh = SetupConfig::fresh();
            match atomic_write_setup(data_root, &fresh) {
                Ok(()) => LoadOutcome::Fresh(fresh),
                Err(_) => LoadOutcome::Abort,
            }
        }
    }
}

/// Read-only inspection of `setup.json` for `not_running_snapshot`.
/// Never classifies and never writes.
pub fn read_setup_read_only(data_root: &Path) -> SetupSnapshot {
    read_setup_snapshot(data_root)
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
    /// The owner's desktop activity switch as applied, not whether it runs.
    running_desktop_activity: bool,
    running_through_pause: bool,
    running_window_titles: bool,
    running_page_addresses: bool,
    failed_sources: Vec<FailedSourceWrite>,
    wizard_save_failed: bool,
    pending_wizard: Option<WizardStep>,
    notice_save_failed: bool,
    login_apply_failed: bool,
    pending_login: Option<bool>,
    pending_through_pause: Option<bool>,
    pending_window_titles: Option<bool>,
    pending_page_addresses: Option<bool>,
    through_pause_save_failed: bool,
    window_titles_save_failed: bool,
    page_addresses_save_failed: bool,
    overlay: HandoffOverlay,
    setup_read_failed: bool,
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
            running_desktop_activity: DESKTOP_ACTIVITY_DEFAULT,
            running_through_pause: DESKTOP_ACTIVITY_THROUGH_PAUSE_DEFAULT,
            running_window_titles: DESKTOP_ACTIVITY_WINDOW_TITLES_DEFAULT,
            running_page_addresses: DESKTOP_ACTIVITY_PAGE_ADDRESSES_DEFAULT,
            failed_sources: Vec::new(),
            wizard_save_failed: false,
            pending_wizard: None,
            notice_save_failed: false,
            login_apply_failed: false,
            pending_login: None,
            pending_through_pause: None,
            pending_window_titles: None,
            pending_page_addresses: None,
            through_pause_save_failed: false,
            window_titles_save_failed: false,
            page_addresses_save_failed: false,
            overlay: HandoffOverlay::default(),
            setup_read_failed: false,
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
        self.durable
            .set_desktop_activity_choices(snapshot.desktop_activity_choices);
        self.running_mic = snapshot.mic;
        self.running_system_audio = snapshot.system_audio;
        self.running_screen = snapshot.screen;
        self.running_desktop_activity = snapshot.desktop_activity_allowed;
        self.running_through_pause = snapshot.desktop_activity_through_pause;
        self.running_window_titles = snapshot.desktop_activity_window_titles;
        self.running_page_addresses = snapshot.desktop_activity_page_addresses;
        self.durable.wizard = snapshot.wizard;
        self.durable.notice_dismissed = snapshot.notice_dismissed;
        self.durable.start_at_login = snapshot.start_at_login;
        self.login_apply_failed = snapshot.login_apply_failed;
        self.setup_read_failed = snapshot.setup_read_failed;
        self.pending_login = None;
        self.pending_through_pause = None;
        self.pending_window_titles = None;
        self.pending_page_addresses = None;
        self.through_pause_save_failed = false;
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
            desktop_activity: desktop_activity_runs(
                self.running_desktop_activity,
                self.running_screen,
                self.running_through_pause,
            ),
            desktop_activity_allowed: self.running_desktop_activity,
            desktop_activity_through_pause: self.running_through_pause,
            desktop_activity_window_titles: self.running_window_titles,
            desktop_activity_page_addresses: self.running_page_addresses,
            desktop_activity_through_pause_save_failed: self.through_pause_save_failed,
            desktop_activity_window_titles_save_failed: self.window_titles_save_failed,
            desktop_activity_page_addresses_save_failed: self.page_addresses_save_failed,
            start_at_login: match self.pending_login {
                Some(choice) => LoginPreference::explicit(choice),
                None => self.durable.start_at_login,
            },
            wizard: self.durable.wizard,
            notice_dismissed: self.durable.notice_dismissed,
            failed_sources: self.failed_sources.clone(),
            wizard_save_failed: self.wizard_save_failed,
            pending_wizard: self.pending_wizard,
            notice_save_failed: self.notice_save_failed,
            login_apply_failed: self.login_apply_failed,
            setup_read_failed: self.setup_read_failed,
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
            desktop_activity_choices: self.durable.desktop_activity_choices(),
        }
    }

    // Any later successful save must carry an OFF choice that previously failed.
    // Failed ON requests remain inactive until their explicit retry.
    fn candidate_with_pending_off(&self) -> SetupConfig {
        let mut candidate = self.durable.clone();
        if self.pending_login == Some(false) {
            candidate.start_at_login = LoginPreference::explicit(false);
        }
        if self.pending_through_pause.is_some() {
            candidate.desktop_activity_through_pause = Some(self.running_through_pause);
        }
        if self.pending_window_titles.is_some() {
            candidate.desktop_activity_window_titles = Some(self.running_window_titles);
        }
        if self.pending_page_addresses.is_some() {
            candidate.desktop_activity_page_addresses = Some(self.running_page_addresses);
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
                SourceKind::DesktopActivity => candidate.desktop_activity = Some(false),
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
        self.setup_read_failed = false;
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
            SourceKind::DesktopActivity => candidate.desktop_activity = Some(enabled),
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
                SourceKind::DesktopActivity => self.durable.desktop_activity = Some(enabled),
            }
            self.remove_failed_source(kind);
        }
    }

    pub fn set_start_at_login(&mut self, enabled: bool) -> bool {
        self.pending_login = Some(enabled);
        let mut candidate = self.candidate_with_pending_off();
        candidate.start_at_login = LoginPreference::explicit(enabled);
        self.persist(candidate)
    }

    pub fn set_login_apply_failed(&mut self, failed: bool) {
        self.login_apply_failed = failed;
        if !failed {
            self.pending_login = None;
        }
    }

    pub fn pending_login(&self) -> Option<bool> {
        if let Some(choice) = self.pending_login {
            Some(choice)
        } else if self.login_apply_failed {
            self.durable.start_at_login.choice()
        } else {
            None
        }
    }

    pub fn set_wizard_step(&mut self, step: WizardStep) -> bool {
        if self.setup_read_failed {
            return false;
        }
        let mut candidate = self.candidate_with_pending_off();
        candidate.wizard = step;
        let saved = self.persist(candidate);
        self.wizard_save_failed = !saved;
        self.pending_wizard = if saved { None } else { Some(step) };
        saved
    }

    pub fn dismiss_notice(&mut self) -> bool {
        if self.setup_read_failed {
            return false;
        }
        let mut candidate = self.candidate_with_pending_off();
        candidate.notice_dismissed = true;
        let saved = self.persist(candidate);
        self.notice_save_failed = !saved;
        saved
    }

    /// Keep desktop activity on through the main pause and with screen off.
    pub fn set_desktop_activity_through_pause(&mut self, enabled: bool) -> bool {
        if !enabled {
            self.running_through_pause = false;
        }
        let mut candidate = self.candidate_with_pending_off();
        candidate.desktop_activity_through_pause = Some(enabled);
        if self.persist(candidate) {
            self.running_through_pause = enabled;
            self.pending_through_pause = None;
            self.through_pause_save_failed = false;
            true
        } else {
            self.pending_through_pause = Some(enabled);
            self.through_pause_save_failed = true;
            false
        }
    }

    pub fn set_desktop_activity_window_titles(&mut self, enabled: bool) -> bool {
        if !enabled {
            self.running_window_titles = false;
        }
        let mut candidate = self.candidate_with_pending_off();
        candidate.desktop_activity_window_titles = Some(enabled);
        if self.persist(candidate) {
            self.running_window_titles = enabled;
            self.pending_window_titles = None;
            self.window_titles_save_failed = false;
            true
        } else {
            self.pending_window_titles = Some(enabled);
            self.window_titles_save_failed = true;
            false
        }
    }

    pub fn set_desktop_activity_page_addresses(&mut self, enabled: bool) -> bool {
        if !enabled {
            self.running_page_addresses = false;
        }
        let mut candidate = self.candidate_with_pending_off();
        candidate.desktop_activity_page_addresses = Some(enabled);
        if self.persist(candidate) {
            self.running_page_addresses = enabled;
            self.pending_page_addresses = None;
            self.page_addresses_save_failed = false;
            true
        } else {
            self.pending_page_addresses = Some(enabled);
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
        if self.setup_read_failed
            && self.failed_sources.is_empty()
            && self.pending_login.is_none()
            && self.pending_through_pause.is_none()
            && self.pending_window_titles.is_none()
            && self.pending_page_addresses.is_none()
        {
            return RetryOutcome {
                started: Vec::new(),
                saved: true,
            };
        }
        let mut candidate = self.durable.clone();
        for fs in &self.failed_sources {
            match fs.kind {
                SourceKind::Screen => candidate.screen = fs.attempted,
                SourceKind::SystemAudio => candidate.system_audio = fs.attempted,
                SourceKind::Mic => candidate.mic = fs.attempted,
                SourceKind::DesktopActivity => candidate.desktop_activity = Some(fs.attempted),
            }
        }
        if let Some(step) = self.pending_wizard {
            candidate.wizard = step;
        }
        if self.notice_save_failed {
            candidate.notice_dismissed = true;
        }
        if let Some(choice) = self.pending_login {
            candidate.start_at_login = LoginPreference::explicit(choice);
        }
        if let Some(choice) = self.pending_through_pause {
            candidate.desktop_activity_through_pause = Some(choice);
        }
        if let Some(choice) = self.pending_window_titles {
            candidate.desktop_activity_window_titles = Some(choice);
        }
        if let Some(choice) = self.pending_page_addresses {
            candidate.desktop_activity_page_addresses = Some(choice);
        }

        let write_ok = if let Some(root) = &self.root {
            atomic_write_setup(root, &candidate).is_ok()
        } else {
            true
        };

        if write_ok {
            self.running_through_pause = candidate.desktop_activity_through_pause();
            self.running_window_titles = candidate.desktop_activity_window_titles();
            self.running_page_addresses = candidate.desktop_activity_page_addresses();
            self.durable = candidate;
            self.pending_through_pause = None;
            self.pending_window_titles = None;
            self.pending_page_addresses = None;
            self.through_pause_save_failed = false;
            self.window_titles_save_failed = false;
            self.page_addresses_save_failed = false;
            self.setup_read_failed = false;
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
    fn a_missing_setup_file_asks_with_every_source_off_whatever_else_is_on_disk() {
        // Files a launch leaves behind are never consent. A first launch (the
        // updater's empty staging id, logs) and an install that lost setup.json
        // both open the wizard with nothing captured or started at login, and
        // the other files are left as they were.
        type Seed = fn(&Path);
        let cases: &[(&str, Seed)] = &[
            ("empty-beta-id", |root| {
                fs::create_dir_all(root.join("packages")).unwrap();
                fs::write(root.join("packages").join(".betaId"), b"").unwrap();
                fs::create_dir_all(root.join("logs")).unwrap();
                fs::write(root.join("logs").join("observer.log"), b"started").unwrap();
            }),
            ("written-beta-id", |root| {
                fs::create_dir_all(root.join("packages")).unwrap();
                fs::write(
                    root.join("packages").join(".betaId"),
                    b"0b6f3c1e-8f2a-4c55-9d0e-2a7b1f4c9e31",
                )
                .unwrap();
            }),
            ("lost-setup", |root| {
                for name in [
                    "pairing.json",
                    "mic.json",
                    "exclusions.json",
                    "hotkey.json",
                    "update.json",
                    "pause.txt",
                ] {
                    fs::write(root.join(name), b"{}").unwrap();
                }
                fs::create_dir_all(root.join("segments").join("20261025")).unwrap();
            }),
        ];
        for (name, seed) in cases {
            let root = temp_test_dir(name);
            seed(&root);
            let mic_before = fs::read(root.join("mic.json")).ok();

            let LoadOutcome::Fresh(config) = load_or_classify(&root) else {
                panic!("{name}: a missing setup.json must open the wizard");
            };
            assert_eq!(config.wizard, WizardStep::Mic, "{name}");
            assert!(
                !(config.mic
                    || config.system_audio
                    || config.screen
                    || config.desktop_activity_enabled()),
                "{name}"
            );
            assert_eq!(
                config.start_at_login,
                LoginPreference::explicit(false),
                "{name}"
            );
            assert_eq!(fs::read(root.join("mic.json")).ok(), mic_before, "{name}");
            assert_eq!(
                load_or_classify(&root),
                LoadOutcome::Valid(SetupConfig::fresh()),
                "{name}"
            );
            let _ = fs::remove_dir_all(&root);
        }
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
                assert_eq!(cfg.start_at_login, LoginPreference::explicit(false));
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
        assert_eq!(
            auth.projection().start_at_login,
            LoginPreference::explicit(true)
        );
        assert_eq!(auth.pending_login(), Some(true));
        assert!(!auth.retry().saved);
        assert!(auth.projection().login_apply_failed);
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        assert!(auth.retry().saved);
        assert_eq!(
            read_readonly(&root).start_at_login,
            LoginPreference::explicit(true)
        );
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
            initial.start_at_login = LoginPreference::explicit(!requested);
            atomic_write_setup(&root, &initial).unwrap();
            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&initial.to_snapshot());
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            assert!(!auth.set_start_at_login(requested));
            auth.set_login_apply_failed(true);
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            assert!(auth.set_wizard_step(WizardStep::Mic));
            assert_eq!(
                read_readonly(&root).start_at_login,
                LoginPreference::explicit(false)
            );
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
    fn failed_off_survives_other_preference_and_failed_reenable_in_both_orders() {
        for titles_first in [true, false] {
            let root = temp_test_dir(if titles_first {
                "off-titles-first"
            } else {
                "off-address-first"
            });
            let mut config = SetupConfig::fresh();
            config.desktop_activity_window_titles = Some(true);
            config.desktop_activity_page_addresses = Some(true);
            atomic_write_setup(&root, &config).unwrap();
            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&config.to_snapshot());
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            if titles_first {
                assert!(!auth.set_desktop_activity_window_titles(false));
                assert!(!auth.set_desktop_activity_page_addresses(false));
                assert!(!auth.set_desktop_activity_window_titles(true));
            } else {
                assert!(!auth.set_desktop_activity_page_addresses(false));
                assert!(!auth.set_desktop_activity_window_titles(false));
                assert!(!auth.set_desktop_activity_page_addresses(true));
            }
            assert!(!auth.projection().desktop_activity_window_titles);
            assert!(!auth.projection().desktop_activity_page_addresses);
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            assert!(auth.retry().saved);
            let LoadOutcome::Valid(saved) = load_or_classify(&root) else {
                panic!("saved config");
            };
            assert_eq!(saved.desktop_activity_window_titles, Some(titles_first));
            assert_eq!(saved.desktop_activity_page_addresses, Some(!titles_first));
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn unrelated_save_preserves_effective_off_after_off_then_on_both_fail() {
        for titles in [false, true] {
            let root = temp_test_dir(if titles {
                "pending-titles-off-on"
            } else {
                "pending-urls-off-on"
            });
            let mut config = SetupConfig::fresh();
            config.desktop_activity_window_titles = Some(true);
            config.desktop_activity_page_addresses = Some(true);
            atomic_write_setup(&root, &config).unwrap();
            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&config.to_snapshot());
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            if titles {
                assert!(!auth.set_desktop_activity_window_titles(false));
                assert!(!auth.set_desktop_activity_window_titles(true));
            } else {
                assert!(!auth.set_desktop_activity_page_addresses(false));
                assert!(!auth.set_desktop_activity_page_addresses(true));
            }
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            assert!(auth.set_wizard_step(WizardStep::Done));
            let LoadOutcome::Valid(saved) = load_or_classify(&root) else {
                panic!("saved config");
            };
            assert_eq!(saved.desktop_activity_window_titles, Some(!titles));
            assert_eq!(saved.desktop_activity_page_addresses, Some(titles));
            assert_eq!(auth.projection().desktop_activity_window_titles, !titles);
            assert_eq!(auth.projection().desktop_activity_page_addresses, titles);
            assert!(auth.retry().saved);
            assert!(
                auth.projection().desktop_activity_window_titles
                    && auth.projection().desktop_activity_page_addresses
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn failed_optional_ons_stay_off_until_retry_saves_the_requested_choices() {
        let root = temp_test_dir("optional-on-retry");
        let mut config = SetupConfig::fresh();
        config.desktop_activity_window_titles = Some(false);
        config.desktop_activity_page_addresses = Some(false);
        atomic_write_setup(&root, &config).unwrap();
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&config.to_snapshot());
        fs::create_dir(root.join("setup.json.tmp")).unwrap();
        assert!(!auth.set_desktop_activity_window_titles(true));
        assert!(!auth.set_desktop_activity_page_addresses(true));
        assert!(!auth.projection().desktop_activity_window_titles);
        assert!(!auth.projection().desktop_activity_page_addresses);
        assert!(!auth.retry().saved);
        assert!(!auth.projection().desktop_activity_window_titles);
        assert!(!auth.projection().desktop_activity_page_addresses);
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        assert!(auth.retry().saved);
        assert!(auth.projection().desktop_activity_window_titles);
        assert!(auth.projection().desktop_activity_page_addresses);
        let LoadOutcome::Valid(saved) = load_or_classify(&root) else {
            panic!("saved config");
        };
        assert!(saved.desktop_activity_window_titles() && saved.desktop_activity_page_addresses());
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn failed_desktop_activity_offs_stay_off_and_retry_persists_both() {
        let root = temp_test_dir("failed-da-prefs");
        let mut initial = SetupConfig::fresh();
        initial.desktop_activity = Some(true);
        initial.desktop_activity_window_titles = Some(true);
        initial.desktop_activity_page_addresses = Some(true);
        atomic_write_setup(&root, &initial).unwrap();

        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&initial.to_snapshot());

        // Block writing to setup.json
        fs::create_dir(root.join("setup.json.tmp")).unwrap();

        // Turning window_titles OFF fails persist
        assert!(!auth.set_desktop_activity_window_titles(false));
        let proj = auth.projection();
        assert!(!proj.desktop_activity_window_titles);
        assert!(proj.desktop_activity_window_titles_save_failed);

        // Turning page_addresses OFF fails persist
        assert!(!auth.set_desktop_activity_page_addresses(false));
        let proj = auth.projection();
        assert!(!proj.desktop_activity_page_addresses);
        assert!(!proj.desktop_activity_window_titles);
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
                assert!(!cfg.desktop_activity_window_titles());
                assert!(!cfg.desktop_activity_page_addresses());
            }
            other => panic!("expected Valid, got {other:?}"),
        }

        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn damaged_setup_retry_saves_each_explicit_optional_preference_without_login_intent() {
        for titles in [true, false] {
            let root = temp_test_dir(if titles {
                "damaged-title-retry"
            } else {
                "damaged-url-retry"
            });
            fs::write(root.join(SETUP_FILE_NAME), b"{malformed").unwrap();
            let StartupLoad::Ready(ready) = startup_load(load_for_startup(&root, true)) else {
                panic!("recoverable startup");
            };
            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&ready.config.to_snapshot());
            auth.setup_read_failed = ready.setup_read_failed;
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            let saved = if titles {
                auth.set_desktop_activity_window_titles(false)
            } else {
                auth.set_desktop_activity_page_addresses(false)
            };
            assert!(!saved);
            assert!(!auth.retry().saved);
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            let outcome = auth.retry();
            assert!(outcome.saved);
            assert!(outcome.started.is_empty());
            let projection = auth.projection();
            assert_eq!(projection.desktop_activity_window_titles, !titles);
            assert_eq!(projection.desktop_activity_page_addresses, titles);
            assert!(
                !projection.desktop_activity_window_titles_save_failed
                    && !projection.desktop_activity_page_addresses_save_failed
            );
            assert!(
                !projection.mic
                    && !projection.system_audio
                    && !projection.screen
                    && !projection.desktop_activity
            );
            assert_eq!(projection.start_at_login.choice(), None);
            let LoadOutcome::Valid(saved) = load_or_classify(&root) else {
                panic!("explicit preference persisted");
            };
            // Only the choice the owner made is saved.
            assert_eq!(
                saved.desktop_activity_window_titles,
                (titles).then_some(false)
            );
            assert_eq!(
                saved.desktop_activity_page_addresses,
                (!titles).then_some(false)
            );
            assert_eq!(saved.start_at_login.choice(), None);
            let mut restarted = SetupAuthority::new();
            restarted.seed(&saved.to_snapshot());
            assert_eq!(restarted.projection().start_at_login.choice(), None);
            assert_eq!(
                restarted.projection().desktop_activity_window_titles,
                !titles
            );
            assert_eq!(
                restarted.projection().desktop_activity_page_addresses,
                titles
            );
            fs::remove_dir_all(root).unwrap();
        }
    }

    #[test]
    fn damaged_setup_and_login_decision_recovery() {
        let my_cmd = r#""C:\app.exe" --from-autostart"#;
        let present_registry = RegistryReadState::Present(my_cmd.into());

        // 1. Damaged cases
        struct DamagedCase {
            name: &'static str,
            setup: fn(&Path),
        }

        let damaged_cases = [
            DamagedCase {
                name: "malformed",
                setup: |root: &Path| {
                    fs::write(root.join(SETUP_FILE_NAME), b"{not json").unwrap();
                },
            },
            DamagedCase {
                name: "directory",
                setup: |root: &Path| {
                    fs::create_dir(root.join(SETUP_FILE_NAME)).unwrap();
                },
            },
            DamagedCase {
                name: "version99",
                setup: |root: &Path| {
                    fs::write(
                        root.join(SETUP_FILE_NAME),
                        br#"{"version":99,"mic":true,"system_audio":true,"screen":true,"start_at_login":true,"wizard":"done","notice_dismissed":true}"#,
                    )
                    .unwrap();
                },
            },
            DamagedCase {
                name: "missing_login",
                setup: |root: &Path| {
                    fs::write(
                        root.join(SETUP_FILE_NAME),
                        br#"{"version":2,"mic":true,"system_audio":true,"screen":true,"wizard":"done","notice_dismissed":true}"#,
                    )
                    .unwrap();
                },
            },
            DamagedCase {
                name: "missing_wizard",
                setup: |root: &Path| {
                    fs::write(
                        root.join(SETUP_FILE_NAME),
                        br#"{"version":2,"start_at_login":true,"mic":true,"system_audio":true,"screen":true,"notice_dismissed":true}"#,
                    )
                    .unwrap();
                },
            },
        ];

        for case in &damaged_cases {
            let root = temp_test_dir(&format!("damaged-{}", case.name));
            (case.setup)(&root);
            let before_bytes = if root.join(SETUP_FILE_NAME).is_file() {
                Some(fs::read(root.join(SETUP_FILE_NAME)).unwrap())
            } else {
                None
            };

            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready, got Abort for {}", case.name),
            };

            assert!(!ready.config.mic);
            assert!(!ready.config.system_audio);
            assert!(!ready.config.screen);
            assert_eq!(ready.login_choice, None);
            assert!(ready.setup_read_failed);

            if let Some(bytes) = before_bytes {
                assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bytes);
            } else {
                assert!(root.join(SETUP_FILE_NAME).is_dir());
            }

            let decision = evaluate_login_decision(
                CopyClass::Installed,
                ready.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(decision, LoginDecision::Leave);
            let _ = fs::remove_dir_all(&root);
        }

        // 2. Separate valid document, explicit "start_at_login": null
        {
            let root = temp_test_dir("valid-null-login");
            let json = br#"{"version":2,"mic":true,"system_audio":false,"screen":false,"start_at_login":null,"wizard":"done","notice_dismissed":true}"#;
            fs::write(root.join(SETUP_FILE_NAME), json).unwrap();

            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };

            assert!(ready.config.mic);
            assert!(!ready.config.system_audio);
            assert!(!ready.config.screen);
            assert_eq!(ready.login_choice, None);
            assert!(!ready.setup_read_failed);
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), json);

            let decision = evaluate_login_decision(
                CopyClass::Installed,
                ready.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(decision, LoginDecision::Leave);
            let _ = fs::remove_dir_all(&root);
        }

        // 3. Valid explicit false on installed/present fixture -> Remove
        {
            let root = temp_test_dir("valid-false-login");
            let json = br#"{"version":2,"mic":true,"system_audio":false,"screen":false,"start_at_login":false,"wizard":"done","notice_dismissed":true}"#;
            fs::write(root.join(SETUP_FILE_NAME), json).unwrap();

            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready.login_choice, Some(false));
            assert!(!ready.setup_read_failed);

            let decision = evaluate_login_decision(
                CopyClass::Installed,
                ready.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(decision, LoginDecision::Remove);
            let _ = fs::remove_dir_all(&root);
        }

        // 4. Valid explicit true
        {
            let root = temp_test_dir("valid-true-login");
            let json = br#"{"version":2,"mic":true,"system_audio":false,"screen":false,"start_at_login":true,"wizard":"done","notice_dismissed":true}"#;
            fs::write(root.join(SETUP_FILE_NAME), json).unwrap();

            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready.login_choice, Some(true));
            assert!(!ready.setup_read_failed);

            // Ensure for Installed
            let dec_inst = evaluate_login_decision(
                CopyClass::Installed,
                ready.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(dec_inst, LoginDecision::Ensure);

            // Leave for Portable when installed_owner_exists Some(true)
            let dec_port_leave = evaluate_login_decision(
                CopyClass::Portable,
                ready.login_choice,
                &present_registry,
                my_cmd,
                Some(true),
            );
            assert_eq!(dec_port_leave, LoginDecision::Leave);

            // Ensure for Portable when Some(false)
            let dec_port_ensure = evaluate_login_decision(
                CopyClass::Portable,
                ready.login_choice,
                &present_registry,
                my_cmd,
                Some(false),
            );
            assert_eq!(dec_port_ensure, LoginDecision::Ensure);
            let _ = fs::remove_dir_all(&root);
        }

        // 5. Fresh: empty temp dir, load_for_startup true, choice Some(false), setup_read_failed false
        {
            let root = temp_test_dir("fresh-startup-load");
            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready.login_choice, Some(false));
            assert!(!ready.setup_read_failed);
            let _ = fs::remove_dir_all(&root);
        }

        // 6. Lost setup.json: mic.json left behind, load_for_startup true, choice Some(false), setup_read_failed false
        {
            let root = temp_test_dir("lost-setup-startup-load");
            fs::write(root.join("mic.json"), b"{}").unwrap();
            let outcome = load_for_startup(&root, true);
            let startup = startup_load(outcome);
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready.login_choice, Some(false));
            assert!(!ready.setup_read_failed);
            let _ = fs::remove_dir_all(&root);
        }

        // 7. Authority sequence on a malformed file
        {
            let root = temp_test_dir("authority-malformed-recovery");
            let bad_bytes = b"{bad json";
            fs::write(root.join(SETUP_FILE_NAME), bad_bytes).unwrap();

            // read_readonly and read_setup_read_only on the original damaged bytes:
            // they do not change bytes and start_at_login is unknown with setup_read_failed true, not explicit false.
            let ro1 = read_readonly(&root);
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(ro1.start_at_login, LoginPreference::unknown());
            assert!(ro1.setup_read_failed);

            let ro2 = read_setup_read_only(&root);
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(ro2.start_at_login, LoginPreference::unknown());
            assert!(ro2.setup_read_failed);

            let startup = startup_load(load_for_startup(&root, true));
            let ready = match startup {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            let mut snapshot = ready.config.to_snapshot();
            snapshot.setup_read_failed = ready.setup_read_failed;

            let mut auth = SetupAuthority::new();
            auth.set_root(root.clone());
            auth.seed(&snapshot);

            // retry, then set_wizard_step or dismiss_notice: original bytes remain, pending_login() is None, setup_read_failed stays true.
            let retry_res = auth.retry();
            assert!(retry_res.saved);
            assert!(retry_res.started.is_empty());
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(auth.pending_login(), None);
            assert!(auth.projection().setup_read_failed);

            assert!(!auth.set_wizard_step(WizardStep::Screen));
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(auth.pending_login(), None);
            assert!(auth.projection().setup_read_failed);

            assert!(!auth.dismiss_notice());
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(auth.pending_login(), None);
            assert!(auth.projection().setup_read_failed);

            // create setup.json.tmp as a directory, set_source Mic true: bytes unchanged, projection login unknown, setup_read_failed true, mic not running.
            fs::create_dir(root.join("setup.json.tmp")).unwrap();
            let mic_running = auth.set_source(SourceKind::Mic, true);
            assert!(!mic_running);
            assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), bad_bytes);
            assert_eq!(auth.projection().start_at_login, LoginPreference::unknown());
            assert!(auth.projection().setup_read_failed);
            assert!(!auth.projection().mic);

            // remove that directory, set_source Mic true.
            fs::remove_dir(root.join("setup.json.tmp")).unwrap();
            let mic_running_now = auth.set_source(SourceKind::Mic, true);
            assert!(mic_running_now);

            // Parse file with serde_json::Value. Do not compare pretty text.
            let saved_val: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join(SETUP_FILE_NAME)).unwrap()).unwrap();
            assert_eq!(saved_val["mic"], serde_json::Value::Bool(true));
            assert_eq!(saved_val["system_audio"], serde_json::Value::Bool(false));
            assert_eq!(saved_val["screen"], serde_json::Value::Bool(false));
            assert_eq!(saved_val["start_at_login"], serde_json::Value::Null);
            assert!(!auth.projection().setup_read_failed);

            // load_for_startup + startup_load again: choice None, Leave on the installed/present fixture.
            let startup2 = startup_load(load_for_startup(&root, true));
            let ready2 = match startup2 {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready2.login_choice, None);
            assert!(!ready2.setup_read_failed);
            let dec2 = evaluate_login_decision(
                CopyClass::Installed,
                ready2.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(dec2, LoginDecision::Leave);

            // set_start_at_login(false). Reload. start_at_login is Bool(false). choice Some(false). Remove on that fixture.
            assert!(auth.set_start_at_login(false));
            let val_after_login: serde_json::Value =
                serde_json::from_slice(&fs::read(root.join(SETUP_FILE_NAME)).unwrap()).unwrap();
            assert_eq!(
                val_after_login["start_at_login"],
                serde_json::Value::Bool(false)
            );

            let startup3 = startup_load(load_for_startup(&root, true));
            let ready3 = match startup3 {
                StartupLoad::Ready(s) => s,
                StartupLoad::Abort => panic!("expected Ready"),
            };
            assert_eq!(ready3.login_choice, Some(false));
            let dec3 = evaluate_login_decision(
                CopyClass::Installed,
                ready3.login_choice,
                &present_registry,
                my_cmd,
                None,
            );
            assert_eq!(dec3, LoginDecision::Remove);

            let _ = fs::remove_dir_all(&root);
        }
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

#[cfg(test)]
mod desktop_activity_settings {
    use super::*;

    fn temp_root(name: &str) -> PathBuf {
        let dir = std::env::temp_dir()
            .join("solstone-setup-activity-tests")
            .join(format!("{}-{}", name, std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn saved_json(root: &Path) -> serde_json::Value {
        serde_json::from_slice(&fs::read(root.join(SETUP_FILE_NAME)).unwrap()).unwrap()
    }

    const ACTIVITY_KEYS: [&str; 4] = [
        "desktop_activity",
        "desktop_activity_through_pause",
        "desktop_activity_window_titles",
        "desktop_activity_page_addresses",
    ];

    fn saved_activity_keys(root: &Path) -> Vec<String> {
        let json = saved_json(root);
        ACTIVITY_KEYS
            .iter()
            .filter(|key| json.get(**key).is_some())
            .map(|key| key.to_string())
            .collect()
    }

    #[test]
    fn activity_follows_screen_and_its_fields_start_on() {
        let mut config = SetupConfig::fresh();
        let snapshot = config.to_snapshot();
        assert!(!snapshot.desktop_activity, "screen off: activity off");
        assert!(snapshot.desktop_activity_allowed);
        assert!(!snapshot.desktop_activity_through_pause);
        assert!(snapshot.desktop_activity_window_titles);
        assert!(snapshot.desktop_activity_page_addresses);

        config.screen = true;
        assert!(
            config.to_snapshot().desktop_activity,
            "screen on: activity on"
        );

        config.desktop_activity = Some(false);
        assert!(
            !config.to_snapshot().desktop_activity,
            "off entirely never runs"
        );
        config.desktop_activity_through_pause = Some(true);
        assert!(!config.to_snapshot().desktop_activity, "off entirely wins");

        config.desktop_activity = None;
        config.screen = false;
        assert!(
            config.to_snapshot().desktop_activity,
            "kept on through pause runs with screen off"
        );
    }

    #[test]
    fn first_run_saves_no_activity_setting() {
        let root = temp_root("first-run");
        let LoadOutcome::Fresh(config) = load_or_classify(&root) else {
            panic!("fresh install");
        };
        assert_eq!(
            config.desktop_activity_choices(),
            DesktopActivityChoices::default()
        );
        assert!(saved_activity_keys(&root).is_empty());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn a_missing_key_reads_as_the_code_default() {
        let root = temp_root("missing-key");
        fs::write(
            root.join(SETUP_FILE_NAME),
            br#"{"version":2,"mic":false,"system_audio":false,"screen":true,"start_at_login":false,"wizard":"done","notice_dismissed":true}"#,
        )
        .unwrap();
        let snapshot = read_readonly(&root);
        assert!(!snapshot.setup_read_failed);
        assert!(snapshot.desktop_activity);
        assert!(snapshot.desktop_activity_window_titles);
        assert!(snapshot.desktop_activity_page_addresses);
        assert!(!snapshot.desktop_activity_through_pause);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn saving_writes_only_what_the_owner_set() {
        let root = temp_root("owner-deltas");
        let LoadOutcome::Fresh(config) = load_or_classify(&root) else {
            panic!("fresh install");
        };
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&config.to_snapshot());

        assert!(auth.set_source(SourceKind::Screen, true));
        assert!(auth.set_wizard_step(WizardStep::Done));
        assert!(saved_activity_keys(&root).is_empty());

        assert!(auth.set_desktop_activity_page_addresses(false));
        assert_eq!(
            saved_activity_keys(&root),
            ["desktop_activity_page_addresses"]
        );
        assert!(auth.set_desktop_activity_through_pause(true));
        assert_eq!(
            saved_activity_keys(&root),
            [
                "desktop_activity_through_pause",
                "desktop_activity_page_addresses"
            ]
        );
        // A choice equal to today's default is still the owner's, and is kept.
        assert!(auth.set_desktop_activity_window_titles(true));
        assert_eq!(saved_json(&root)["desktop_activity_window_titles"], true);

        let projection = auth.projection();
        assert!(projection.desktop_activity);
        assert!(projection.desktop_activity_through_pause);
        assert!(!projection.desktop_activity_page_addresses);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn turning_activity_off_entirely_is_saved_and_never_runs() {
        let root = temp_root("off-entirely");
        let mut config = SetupConfig::fresh();
        config.screen = true;
        atomic_write_setup(&root, &config).unwrap();
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&config.to_snapshot());
        assert!(auth.projection().desktop_activity);

        assert!(!auth.set_source(SourceKind::DesktopActivity, false));
        assert!(!auth.projection().desktop_activity);
        assert!(!auth.projection().desktop_activity_allowed);
        assert_eq!(saved_json(&root)["desktop_activity"], false);

        let LoadOutcome::Valid(saved) = load_or_classify(&root) else {
            panic!("saved config");
        };
        assert!(!saved.to_snapshot().desktop_activity);
        let _ = fs::remove_dir_all(&root);
    }

    /// A version 1 file as the previous release wrote it at first run.
    const VERSION_1: &[u8] = br#"{
  "version": 1,
  "mic": true,
  "system_audio": false,
  "screen": true,
  "start_at_login": true,
  "wizard": "done",
  "notice_dismissed": true,
  "desktop_activity": false,
  "desktop_activity_window_titles": false,
  "desktop_activity_page_addresses": false
}"#;

    #[test]
    fn existing_installs_move_to_the_new_defaults_once() {
        let root = temp_root("upgrade-once");
        fs::write(root.join(SETUP_FILE_NAME), VERSION_1).unwrap();

        let LoadOutcome::Valid(upgraded) = load_or_classify(&root) else {
            panic!("an older version is upgraded, not failed closed");
        };
        assert_eq!(upgraded.version, CURRENT_SETUP_VERSION);
        assert!(upgraded.to_snapshot().desktop_activity, "follows screen");
        assert!(upgraded.desktop_activity_window_titles());
        assert!(upgraded.desktop_activity_page_addresses());
        // Nothing else moves.
        assert!(upgraded.mic && !upgraded.system_audio && upgraded.screen);
        assert_eq!(upgraded.start_at_login, LoginPreference::explicit(true));
        assert_eq!(upgraded.wizard, WizardStep::Done);
        assert!(upgraded.notice_dismissed);
        // The upgrade is saved, without the discarded values.
        assert_eq!(saved_json(&root)["version"], CURRENT_SETUP_VERSION);
        assert!(saved_activity_keys(&root).is_empty());

        // The owner turns titles and activity off after the upgrade.
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&upgraded.to_snapshot());
        assert!(auth.set_desktop_activity_window_titles(false));
        assert!(!auth.set_source(SourceKind::DesktopActivity, false));

        // Later launches keep those choices: the upgrade does not run again.
        for _ in 0..2 {
            let LoadOutcome::Valid(again) = load_or_classify(&root) else {
                panic!("saved config");
            };
            assert_eq!(again.desktop_activity, Some(false));
            assert_eq!(again.desktop_activity_window_titles, Some(false));
            assert!(!again.to_snapshot().desktop_activity);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn read_only_paths_upgrade_in_memory_without_writing() {
        let root = temp_root("upgrade-read-only");
        fs::write(root.join(SETUP_FILE_NAME), VERSION_1).unwrap();

        let snapshot = read_readonly(&root);
        assert!(!snapshot.setup_read_failed);
        assert!(snapshot.desktop_activity);
        assert!(snapshot.desktop_activity_window_titles);
        assert!(snapshot.desktop_activity_page_addresses);
        let LoadOutcome::Valid(dark) = load_for_startup(&root, false) else {
            panic!("dark builds read the older version");
        };
        assert_eq!(
            dark.desktop_activity_choices(),
            DesktopActivityChoices::default()
        );
        assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), VERSION_1);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn an_upgrade_that_cannot_be_saved_runs_again_next_launch() {
        let root = temp_root("upgrade-unsaved");
        fs::write(root.join(SETUP_FILE_NAME), VERSION_1).unwrap();
        fs::create_dir(root.join("setup.json.tmp")).unwrap();
        let LoadOutcome::Valid(upgraded) = load_or_classify(&root) else {
            panic!("upgraded in memory");
        };
        assert!(upgraded.to_snapshot().desktop_activity);
        assert_eq!(fs::read(root.join(SETUP_FILE_NAME)).unwrap(), VERSION_1);
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        let LoadOutcome::Valid(_) = load_or_classify(&root) else {
            panic!("upgraded");
        };
        assert_eq!(saved_json(&root)["version"], CURRENT_SETUP_VERSION);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn failed_through_pause_on_stays_off_until_retry_saves_it() {
        let root = temp_root("through-pause-retry");
        let config = SetupConfig::fresh();
        atomic_write_setup(&root, &config).unwrap();
        let mut auth = SetupAuthority::new();
        auth.set_root(root.clone());
        auth.seed(&config.to_snapshot());
        fs::create_dir(root.join("setup.json.tmp")).unwrap();
        assert!(!auth.set_desktop_activity_through_pause(true));
        let projection = auth.projection();
        assert!(!projection.desktop_activity_through_pause);
        assert!(!projection.desktop_activity);
        assert!(projection.desktop_activity_through_pause_save_failed);
        fs::remove_dir(root.join("setup.json.tmp")).unwrap();
        assert!(auth.retry().saved);
        let projection = auth.projection();
        assert!(projection.desktop_activity_through_pause);
        assert!(projection.desktop_activity);
        assert!(!projection.desktop_activity_through_pause_save_failed);
        assert_eq!(saved_json(&root)["desktop_activity_through_pause"], true);
        let _ = fs::remove_dir_all(&root);
    }
}
