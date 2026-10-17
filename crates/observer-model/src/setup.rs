// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Durable setup and source preferences (`setup.json`).
//!
//! Owns the classification of fresh vs legacy installs, the pure login
//! decision evaluation, and atomic load/store of `%LocalAppData%\Solstone\setup.json`.

#![forbid(unsafe_code)]

use std::fs::{self, File};
use std::io::{self, Write};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::SourceKind;

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
        }
    }

    pub fn to_snapshot(&self) -> SetupSnapshot {
        SetupSnapshot {
            mic: self.mic,
            system_audio: self.system_audio,
            screen: self.screen,
            start_at_login: self.start_at_login,
            wizard: self.wizard,
            notice_dismissed: self.notice_dismissed,
            unsaved: None,
            login_apply_failed: false,
        }
    }
}

/// Marker recording an attempted source toggle that could not be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnsavedChoice {
    pub kind: SourceKind,
    pub attempted: bool,
}

/// Honest setup snapshot folded into HealthDump.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SetupSnapshot {
    pub mic: bool,
    pub system_audio: bool,
    pub screen: bool,
    pub start_at_login: bool,
    pub wizard: WizardStep,
    pub notice_dismissed: bool,
    #[serde(default)]
    pub unsaved: Option<UnsavedChoice>,
    #[serde(default)]
    pub login_apply_failed: bool,
}

impl Default for SetupSnapshot {
    fn default() -> Self {
        Self {
            mic: false,
            system_audio: false,
            screen: false,
            start_at_login: false,
            wizard: WizardStep::Done,
            notice_dismissed: true,
            unsaved: None,
            login_apply_failed: false,
        }
    }
}

impl SetupSnapshot {
    pub fn is_enabled(&self, kind: SourceKind) -> bool {
        match kind {
            SourceKind::Screen => self.screen,
            SourceKind::SystemAudio => self.system_audio,
            SourceKind::Mic => self.mic,
        }
    }

    pub fn any_enabled(&self) -> bool {
        self.mic || self.system_audio || self.screen
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

/// Pure result of applying a source toggle intent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChoiceApplyResult {
    pub saved: bool,
    pub running: bool,
    pub unsaved: Option<UnsavedChoice>,
}

/// Apply a source choice given the saved value, requested value, and disk write status.
pub fn apply_source_choice(
    kind: SourceKind,
    saved_bool: bool,
    requested_bool: bool,
    write_succeeded: bool,
) -> ChoiceApplyResult {
    if write_succeeded {
        ChoiceApplyResult {
            saved: requested_bool,
            running: requested_bool,
            unsaved: None,
        }
    } else {
        ChoiceApplyResult {
            saved: saved_bool,
            running: false,
            unsaved: Some(UnsavedChoice {
                kind,
                attempted: requested_bool,
            }),
        }
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
    fn missing_file_with_paired_fixture_migrates_to_legacy() {
        let root = temp_test_dir("legacy-paired");
        fs::write(root.join("pairing.json"), b"{\"some\":\"pairing\"}").unwrap();

        let outcome = load_or_classify(&root);
        assert_eq!(outcome, LoadOutcome::Legacy(SetupConfig::legacy()));
        assert!(root.join("pairing.json").is_file());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_file_with_unpaired_prior_use_migrates_to_legacy() {
        let root = temp_test_dir("legacy-unpaired");
        fs::write(root.join("mic.json"), b"{\"mic\":\"default\"}").unwrap();
        fs::write(root.join("pause.txt"), b"until-resumed\n").unwrap();

        let outcome = load_or_classify(&root);
        assert_eq!(outcome, LoadOutcome::Legacy(SetupConfig::legacy()));
        assert!(root.join("mic.json").is_file());
        assert!(root.join("pause.txt").is_file());
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
    fn choice_apply_matrix() {
        // on + write ok
        let r1 = apply_source_choice(SourceKind::Screen, false, true, true);
        assert_eq!(
            r1,
            ChoiceApplyResult {
                saved: true,
                running: true,
                unsaved: None
            }
        );

        // on + write fail
        let r2 = apply_source_choice(SourceKind::Screen, false, true, false);
        assert_eq!(
            r2,
            ChoiceApplyResult {
                saved: false,
                running: false,
                unsaved: Some(UnsavedChoice {
                    kind: SourceKind::Screen,
                    attempted: true
                })
            }
        );

        // off + write ok
        let r3 = apply_source_choice(SourceKind::Screen, true, false, true);
        assert_eq!(
            r3,
            ChoiceApplyResult {
                saved: false,
                running: false,
                unsaved: None
            }
        );

        // off + write fail
        let r4 = apply_source_choice(SourceKind::Screen, true, false, false);
        assert_eq!(
            r4,
            ChoiceApplyResult {
                saved: true,
                running: false,
                unsaved: Some(UnsavedChoice {
                    kind: SourceKind::Screen,
                    attempted: false
                })
            }
        );
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
}
