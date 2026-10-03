// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Pure About rendering and the small protocol projections shared by the app.

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[cfg(test)]
const ABOUT_ADOPTION: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../contracts/solstone-core-about/adoption.json"
);
#[cfg(test)]
const ABOUT_REPOSITORY: &str = "https://github.com/solpbc/solstone-journal";
#[cfg(test)]
const ABOUT_COMMIT: &str = "ec1983799b66d3616708851d01803e4f3d6f0a20";
#[cfg(test)]
const ABOUT_MANIFEST_SHA256: &str =
    "301c1d84616379e11aaf22bb341e524bb4b2317051ea08453db9fdd87c706ab0";
#[cfg(test)]
const ABOUT_FILES: [&str; 6] = [
    "about.schema.json",
    "contract.json",
    "manifest.json",
    "native-about.json",
    "native-about.schema.json",
    "resources.json",
];

/// The producer's freshness input. `Current` deliberately has no suffix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Freshness {
    Current,
    Stale {
        seen_at_epoch_secs: Option<u64>,
        now_epoch_secs: u64,
    },
}

/// Host facts projected from `/api/system/about`. Unknown resource properties,
/// including the server's `about` string, are intentionally not represented.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct JournalAboutFacts {
    pub version: String,
    #[serde(default)]
    pub build: Option<String>,
    pub os: String,
    pub os_version: String,
    pub arch: String,
}

/// Raw, platform-tier observations consumed by the pure Windows mapper.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowsObservation {
    pub build_number: Option<String>,
    pub native_machine: Option<u16>,
}

/// Closed native-host `about` value. Its serialized field set is exactly seven
/// keys; validation is separate because several rules span fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NativeAboutSnapshot {
    pub protocol_version: u32,
    pub os: String,
    pub os_version: String,
    pub arch: String,
    pub journal_line: String,
    pub journal_current: bool,
    pub journal_seen_at_epoch_secs: Option<u64>,
}

impl NativeAboutSnapshot {
    pub fn new(
        os: impl Into<String>,
        os_version: impl Into<String>,
        arch: impl Into<String>,
        journal_line: impl Into<String>,
        journal_current: bool,
        journal_seen_at_epoch_secs: Option<u64>,
    ) -> Result<Self, String> {
        let snapshot = Self {
            protocol_version: 1,
            os: os.into(),
            os_version: os_version.into(),
            arch: arch.into(),
            journal_line: journal_line.into(),
            journal_current,
            journal_seen_at_epoch_secs,
        };
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn unknown(
        os: impl Into<String>,
        os_version: impl Into<String>,
        arch: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: 1,
            os: os.into(),
            os_version: os_version.into(),
            arch: arch.into(),
            journal_line: unknown_journal_line(),
            journal_current: false,
            journal_seen_at_epoch_secs: None,
        }
    }

    pub fn from_value(value: &Value) -> Result<Self, String> {
        let object = value
            .as_object()
            .ok_or_else(|| "about snapshot must be an object".to_string())?;
        if object.len() != 7 {
            return Err("about snapshot must contain exactly seven keys".into());
        }
        let snapshot: Self = serde_json::from_value(value.clone())
            .map_err(|_| "invalid about snapshot shape".to_string())?;
        snapshot.validate()?;
        Ok(snapshot)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.protocol_version != 1 {
            return Err("unsupported about protocol version".into());
        }
        if self
            .os
            .chars()
            .chain(self.os_version.chars())
            .chain(self.arch.chars())
            .chain(self.journal_line.chars())
            .any(char::is_control)
        {
            return Err("about snapshot contains a control character".into());
        }
        if self.journal_line.contains("\n")
            || self.journal_line.contains("\r")
            || self.journal_line.contains(" · last seen ")
            || self.journal_line.contains(" (last known)")
        {
            return Err("native journal line must be the base line".into());
        }
        if self.journal_line == unknown_journal_line()
            && (self.journal_current || self.journal_seen_at_epoch_secs.is_some())
        {
            return Err("unknown journal cannot be current or have a seen-at time".into());
        }
        if !self.journal_line.starts_with("journal ") || self.journal_line == "journal " {
            return Err("native journal line has an invalid name".into());
        }
        Ok(())
    }
}

/// Render one line from the supplied facts. Empty fact groups and their
/// separator are omitted; no server-provided display string is accepted here.
pub fn render_line(
    name: &str,
    version: Option<&str>,
    build: Option<&str>,
    os: Option<&str>,
    os_version: Option<&str>,
    arch: Option<&str>,
    freshness: Freshness,
) -> String {
    let version = version.filter(|value| !value.is_empty());
    let Some(version) = version else {
        return if name == "journal" {
            unknown_journal_line()
        } else {
            name.to_owned()
        };
    };
    let version = version.strip_prefix('v').unwrap_or(version);
    if version.is_empty() {
        return if name == "journal" {
            unknown_journal_line()
        } else {
            name.to_owned()
        };
    }
    let mut line = format!("{name} {version}");
    if let Some(build) = build.filter(|value| !value.is_empty()) {
        line.push_str(" (");
        line.push_str(build);
        line.push(')');
    }

    let platform = [os, os_version]
        .into_iter()
        .flatten()
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    if !platform.is_empty() {
        line.push_str(" · ");
        line.push_str(&platform);
    }
    if let Some(arch) = arch.filter(|value| !value.is_empty()) {
        line.push_str(" · ");
        line.push_str(normalize_arch(arch));
    }
    if let Freshness::Stale {
        seen_at_epoch_secs: Some(seen_at),
        now_epoch_secs,
    } = freshness
    {
        if seen_at <= now_epoch_secs {
            line.push_str(" · last seen ");
            line.push_str(&relative_age(now_epoch_secs - seen_at));
        }
    }
    line
}

pub fn render_journal_line(
    version: Option<&str>,
    facts: Option<&JournalAboutFacts>,
    fresh: bool,
    seen_at_epoch_secs: Option<u64>,
    now_epoch_secs: u64,
) -> String {
    let freshness = if fresh {
        Freshness::Current
    } else {
        Freshness::Stale {
            seen_at_epoch_secs,
            now_epoch_secs,
        }
    };
    render_line(
        "journal",
        version,
        facts.and_then(|facts| facts.build.as_deref()),
        facts.map(|facts| facts.os.as_str()),
        facts.map(|facts| facts.os_version.as_str()),
        facts.map(|facts| facts.arch.as_str()),
        freshness,
    )
}

pub fn render_journal_base_line(
    version: Option<&str>,
    facts: Option<&JournalAboutFacts>,
) -> String {
    render_line(
        "journal",
        version,
        facts.and_then(|facts| facts.build.as_deref()),
        facts.map(|facts| facts.os.as_str()),
        facts.map(|facts| facts.os_version.as_str()),
        facts.map(|facts| facts.arch.as_str()),
        Freshness::Current,
    )
}

pub fn decode_journal_resource(body: &[u8]) -> Result<JournalAboutFacts, String> {
    #[derive(Deserialize)]
    struct Resource {
        protocol_version: u32,
        version: String,
        #[serde(default)]
        build: Option<String>,
        os: String,
        os_version: String,
        arch: String,
    }

    let resource: Resource =
        serde_json::from_slice(body).map_err(|_| "invalid about resource".to_string())?;
    if resource.protocol_version != 1
        || !valid_fact(&resource.version, 128)
        || normalize_version(&resource.version).is_empty()
        || !valid_text(&resource.os, 64)
        || !valid_text(&resource.os_version, 64)
        || !valid_text(&resource.arch, 64)
        || resource
            .build
            .as_deref()
            .is_some_and(|build| !valid_text(build, 128))
    {
        return Err("invalid about resource facts".into());
    }
    Ok(JournalAboutFacts {
        version: resource.version,
        build: resource.build,
        os: resource.os,
        os_version: resource.os_version,
        arch: resource.arch,
    })
}

pub fn normalize_version(version: &str) -> &str {
    version.strip_prefix('v').unwrap_or(version)
}

pub fn windows_os_version(build_number: Option<&str>) -> String {
    let Some(build) = build_number.filter(|build| !build.is_empty()) else {
        return String::new();
    };
    let Ok(number) = build.parse::<u32>() else {
        return String::new();
    };
    format!("{} {build}", if number >= 22_000 { "11" } else { "10" })
}

pub fn windows_arch(native_machine: Option<u16>) -> Option<&'static str> {
    match native_machine? {
        0xAA64 => Some("arm64"),
        0x8664 => Some("x86_64"),
        0x014C => Some("x86"),
        0x01C4 => Some("arm"),
        _ => None,
    }
}

pub fn render_windows_app_line(package_version: &str, observation: &WindowsObservation) -> String {
    let os_version = windows_os_version(observation.build_number.as_deref());
    let arch = windows_arch(observation.native_machine);
    render_line(
        "windows app",
        Some(package_version),
        None,
        Some("windows"),
        Some(&os_version),
        arch,
        Freshness::Current,
    )
}

pub fn native_windows_snapshot(
    observation: &WindowsObservation,
    journal_base_line: &str,
    journal_current: bool,
    journal_seen_at_epoch_secs: Option<u64>,
) -> NativeAboutSnapshot {
    let os_version = windows_os_version(observation.build_number.as_deref());
    let arch = windows_arch(observation.native_machine).unwrap_or("");
    NativeAboutSnapshot::new(
        "windows",
        os_version.clone(),
        arch,
        journal_base_line,
        journal_current,
        journal_seen_at_epoch_secs,
    )
    .unwrap_or_else(|_| NativeAboutSnapshot::unknown("windows", os_version, arch))
}

pub fn compose_about_block(app_line: &str, journal_line: &str) -> String {
    format!("{app_line}\n{journal_line}")
}

pub fn unknown_journal_line() -> String {
    "journal unknown".to_owned()
}

fn normalize_arch(arch: &str) -> &str {
    match arch {
        "aarch64" | "ARM64" | "arm64" | "arm64-v8a" => "arm64",
        "amd64" | "x64" | "AMD64" | "x86_64" => "x86_64",
        other => other,
    }
}

fn valid_fact(value: &str, max_bytes: usize) -> bool {
    !value.trim().is_empty() && valid_text(value, max_bytes)
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    value.len() <= max_bytes
        && !value
            .chars()
            .any(|character| character.is_control() || matches!(character, '\u{2028}' | '\u{2029}'))
}

fn relative_age(age_secs: u64) -> String {
    if age_secs < 60 {
        "just now".to_owned()
    } else if age_secs < 3_600 {
        let minutes = age_secs / 60;
        format!(
            "{minutes} minute{} ago",
            if minutes == 1 { "" } else { "s" }
        )
    } else if age_secs < 86_400 {
        let hours = age_secs / 3_600;
        format!("{hours} hour{} ago", if hours == 1 { "" } else { "s" })
    } else {
        let days = age_secs / 86_400;
        format!("{days} day{} ago", if days == 1 { "" } else { "s" })
    }
}

/// Build the existing report fragment contract, now carrying the same frozen
/// About block shown and copied by the UI.
pub fn build_report_url(dump: &crate::HealthDump, os_version: Option<&str>) -> String {
    const HELP_URL: &str = "https://support.solstone.app";
    let error_code = dump
        .sync
        .upload
        .last_error_reason
        .as_deref()
        .or_else(|| dump.sources.iter().find_map(source_error_code));
    let state: &'static str = dump.app_state.into();
    let mut fields = vec![
        ("report", "v1".to_owned()),
        ("app", "solstone for windows".to_owned()),
    ];
    if !dump.version.is_empty() {
        fields.push(("version", dump.version.chars().take(120).collect()));
    }
    fields.push(("os", "windows".to_owned()));
    if let Some(os_version) = os_version.filter(|value| !value.is_empty()) {
        fields.push(("os_version", os_version.chars().take(120).collect()));
    }
    if let Some(code) = error_code {
        fields.push(("error_code", code.chars().take(200).collect()));
    } else {
        fields.push(("state", state.chars().take(500).collect()));
    }
    let recent = recent_state_lines(dump);
    if !recent.is_empty() {
        fields.push(("recent", recent.chars().take(4000).collect()));
    }
    fields.push(("about", dump.sync.about_block.clone()));
    let fragment = fields
        .into_iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{HELP_URL}/#{fragment}")
}

fn source_error_code(source: &crate::SourceReport) -> Option<&'static str> {
    match &source.state {
        crate::SourceState::Faulted { reason, .. } => Some((*reason).into()),
        _ => None,
    }
}

fn recent_state_lines(dump: &crate::HealthDump) -> String {
    let mut lines = dump
        .sources
        .iter()
        .map(|source| {
            let kind: &'static str = source.kind.into();
            let state = match &source.state {
                crate::SourceState::Active => "active",
                crate::SourceState::Inactive => "inactive",
                crate::SourceState::NoInputDevice => "no_input_device",
                crate::SourceState::Faulted { reason, .. } => (*reason).into(),
            };
            format!("{kind}: {state}")
        })
        .collect::<Vec<_>>();
    if let Some(code) = dump.sync.upload.last_error_reason.as_deref() {
        lines.push(format!("sync: {code}"));
    }
    lines.join("\n")
}

fn form_encode(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        if byte == b' ' {
            encoded.push('+');
        } else if byte.is_ascii_alphanumeric() || matches!(byte, b'*' | b'-' | b'.' | b'_') {
            encoded.push(char::from(byte));
        } else {
            use std::fmt::Write as _;
            write!(&mut encoded, "%{byte:02X}").expect("writing to a string cannot fail");
        }
    }
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sha2::{Digest, Sha256};

    fn about_root() -> std::path::PathBuf {
        std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../contracts/solstone-core-about")
    }

    fn sha256(bytes: &[u8]) -> String {
        format!("{:x}", Sha256::digest(bytes))
    }

    #[test]
    fn renderer_uses_contract_fixtures_and_aliases() {
        let contract: Value = serde_json::from_str(include_str!(
            "../../../contracts/solstone-core-about/bundle/contract.json"
        ))
        .unwrap();
        for fixture in contract["fixtures"].as_array().unwrap() {
            let line = render_line(
                "journal",
                fixture["version"].as_str(),
                fixture["build"].as_str(),
                fixture["os"].as_str(),
                fixture["os_version"].as_str(),
                fixture["arch"].as_str(),
                Freshness::Current,
            );
            assert_eq!(line, fixture["about"].as_str().unwrap());
        }
        assert_eq!(
            render_line(
                "journal",
                Some("vv1"),
                None,
                None,
                None,
                Some("x64"),
                Freshness::Current
            ),
            "journal v1 · x86_64"
        );
        assert_eq!(
            render_journal_line(None, None, false, None, 123),
            "journal unknown"
        );
        assert_eq!(
            render_journal_line(Some("v"), None, false, Some(1), 123),
            "journal unknown"
        );
        let journal_base_line = render_journal_base_line(Some("v"), None);
        assert_eq!(journal_base_line, "journal unknown");
        let native = native_windows_snapshot(
            &WindowsObservation::default(),
            &journal_base_line,
            true,
            Some(123),
        );
        assert_eq!(native.journal_line, "journal unknown");
        assert!(!native.journal_current);
        assert_eq!(native.journal_seen_at_epoch_secs, None);
        assert!(NativeAboutSnapshot::new("windows", "", "", "journal ", false, None).is_err());
    }

    #[test]
    fn resource_decoder_rejects_version_empty_after_prefix_removal() {
        let resource = serde_json::json!({
            "protocol_version": 1,
            "version": "v",
            "os": "ubuntu",
            "os_version": "24.04",
            "arch": "x86_64",
        });
        assert!(decode_journal_resource(&serde_json::to_vec(&resource).unwrap()).is_err());
    }

    #[test]
    fn stale_age_is_frozen_and_uses_the_locked_thresholds() {
        let facts = JournalAboutFacts {
            version: "1.2.3".into(),
            build: None,
            os: "ubuntu".into(),
            os_version: "24.04".into(),
            arch: "x86_64".into(),
        };
        let cases = [
            (0, "just now"),
            (60, "1 minute ago"),
            (120, "2 minutes ago"),
            (3_600, "1 hour ago"),
            (7_200, "2 hours ago"),
            (86_400, "1 day ago"),
            (259_200, "3 days ago"),
        ];
        for (age, relative) in cases {
            assert_eq!(
                render_journal_line(Some("1.2.3"), Some(&facts), false, Some(1_000), 1_000 + age),
                format!("journal 1.2.3 · ubuntu 24.04 · x86_64 · last seen {relative}")
            );
        }
        assert_eq!(
            render_journal_line(Some("1.2.3"), None, false, Some(880), 1_000),
            "journal 1.2.3 · last seen 2 minutes ago"
        );
        assert_eq!(
            render_journal_line(Some("1.2.3"), Some(&facts), false, Some(1_001), 1_000),
            "journal 1.2.3 · ubuntu 24.04 · x86_64"
        );
        assert_eq!(
            render_journal_line(Some("1.2.3"), Some(&facts), false, None, 1_000),
            "journal 1.2.3 · ubuntu 24.04 · x86_64"
        );
        assert_eq!(
            render_journal_line(Some("1.2.3"), Some(&facts), true, Some(1), 1_000),
            "journal 1.2.3 · ubuntu 24.04 · x86_64"
        );
    }

    #[test]
    fn maps_windows_observations_from_the_native_machine() {
        assert_eq!(windows_os_version(Some("22000")), "11 22000");
        assert_eq!(windows_os_version(Some("19045")), "10 19045");
        assert_eq!(windows_os_version(None), "");
        assert_eq!(windows_os_version(Some("unknown")), "");
        assert_eq!(windows_arch(Some(0xAA64)), Some("arm64"));
        assert_eq!(windows_arch(Some(0x8664)), Some("x86_64"));
        assert_eq!(windows_arch(Some(0x014C)), Some("x86"));
        assert_eq!(windows_arch(Some(0x01C4)), Some("arm"));
        assert_eq!(windows_arch(Some(0x1234)), None);
        assert_eq!(windows_arch(None), None);
        assert_eq!(
            render_windows_app_line("2.0.16", &WindowsObservation::default()),
            "windows app 2.0.16 · windows"
        );
        assert_eq!(
            render_windows_app_line(
                "2.0.16",
                &WindowsObservation {
                    build_number: Some("26100".into()),
                    native_machine: Some(0xAA64),
                }
            ),
            "windows app 2.0.16 · windows 11 26100 · arm64"
        );
    }

    #[test]
    fn bundle_hashes_and_adoption_pin_match_committed_bytes() {
        let root = about_root();
        let manifest_bytes = std::fs::read(root.join("bundle/manifest.json")).unwrap();
        assert_eq!(sha256(&manifest_bytes), ABOUT_MANIFEST_SHA256);
        let manifest: Value = serde_json::from_slice(&manifest_bytes).unwrap();
        let adoption: Value =
            serde_json::from_slice(&std::fs::read(ABOUT_ADOPTION).unwrap()).unwrap();
        assert_eq!(adoption["authority_repository"], ABOUT_REPOSITORY);
        assert_eq!(adoption["authority_commit"], ABOUT_COMMIT);
        assert_eq!(adoption["authority_manifest_sha256"], ABOUT_MANIFEST_SHA256);
        assert_eq!(
            adoption["bundle_files"].as_array().unwrap().len(),
            ABOUT_FILES.len()
        );
        for file in ABOUT_FILES {
            let digest = sha256(&std::fs::read(root.join("bundle").join(file)).unwrap());
            let manifest_digest = if file == "manifest.json" {
                ABOUT_MANIFEST_SHA256
            } else {
                manifest["artifacts"][file].as_str().unwrap()
            };
            let adopted_digest = adoption["bundle_files"]
                .as_array()
                .unwrap()
                .iter()
                .find(|entry| entry["path"] == file)
                .unwrap()["sha256"]
                .as_str()
                .unwrap();
            assert_eq!(digest, manifest_digest, "manifest hash for {file}");
            assert_eq!(digest, adopted_digest, "adoption hash for {file}");
        }
    }

    #[test]
    fn resource_fixtures_drop_unknown_privacy_fields() {
        let root = about_root();
        let resources: Value =
            serde_json::from_slice(&std::fs::read(root.join("bundle/resources.json")).unwrap())
                .unwrap();
        for resource in resources["valid"].as_array().unwrap() {
            let facts = decode_journal_resource(&serde_json::to_vec(resource).unwrap()).unwrap();
            let rendered = render_journal_line(Some(&facts.version), Some(&facts), true, None, 0);
            assert_eq!(rendered, resource["about"]);
        }
        for resource in resources["invalid"].as_array().unwrap() {
            assert!(decode_journal_resource(&serde_json::to_vec(resource).unwrap()).is_err());
        }

        let mut populated = resources["valid"][0].clone();
        let object = populated.as_object_mut().unwrap();
        for (key, value) in [
            ("hostname", "PRIVATE HOST"),
            ("device_name", "DEVICE NAME"),
            ("path", "C:/private/path"),
            ("account_id", "ACCOUNT-ID"),
            ("address", "192.0.2.1"),
            ("model_name", "MODEL NAME"),
        ] {
            object.insert(key.to_owned(), json!(value));
        }
        let facts = decode_journal_resource(&serde_json::to_vec(&populated).unwrap()).unwrap();
        let block = compose_about_block(
            "windows app 2.0.16 · windows 11 26100 · x86_64",
            &render_journal_line(Some(&facts.version), Some(&facts), true, None, 0),
        );
        for private in [
            "PRIVATE HOST",
            "DEVICE NAME",
            "C:/private/path",
            "ACCOUNT-ID",
            "192.0.2.1",
            "MODEL NAME",
        ] {
            assert!(!block.contains(private));
        }
    }

    #[test]
    fn native_snapshot_fixtures_are_closed_and_keep_future_envelope_roots() {
        let root = about_root();
        let native: Value =
            serde_json::from_slice(&std::fs::read(root.join("bundle/native-about.json")).unwrap())
                .unwrap();
        for snapshot in native["valid"].as_array().unwrap() {
            NativeAboutSnapshot::from_value(snapshot).unwrap();
        }
        for snapshot in native["invalid"].as_array().unwrap() {
            assert!(NativeAboutSnapshot::from_value(snapshot).is_err());
        }
        for envelope in native["envelopes"].as_array().unwrap() {
            if let Some(snapshot) = envelope.get("about") {
                if snapshot.get("hostname").is_some() {
                    assert!(NativeAboutSnapshot::from_value(snapshot).is_err());
                } else {
                    NativeAboutSnapshot::from_value(snapshot).unwrap();
                }
            }
        }
        let future = native["envelopes"]
            .as_array()
            .unwrap()
            .iter()
            .find(|envelope| envelope.get("future_root").is_some())
            .unwrap();
        assert!(NativeAboutSnapshot::from_value(&future["about"]).is_ok());
        assert_eq!(ABOUT_FILES.len(), 6);
    }

    #[test]
    fn report_fragment_round_trips_the_frozen_block() {
        assert_eq!(form_encode("a b+c~\n"), "a+b%2Bc%7E%0A");

        let facts = JournalAboutFacts {
            version: "1.2.3".into(),
            build: None,
            os: "ubuntu".into(),
            os_version: "24.04".into(),
            arch: "x86_64".into(),
        };
        let frozen_journal_line =
            render_journal_line(Some("1.2.3"), Some(&facts), false, Some(1_000), 1_120);
        let frozen_about_block =
            compose_about_block("windows app 2.0.16 · windows", &frozen_journal_line);
        let dump = crate::HealthDump {
            app_state: crate::AppPhase::Paused,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: true,
            version: "2.0.16".into(),
            sync: crate::SyncSnapshot {
                about_block: frozen_about_block.clone(),
                ..Default::default()
            },
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: Default::default(),
            pump_degraded: false,
            listener_faults: crate::ListenerFaults::default(),
        };
        let url = build_report_url(&dump, Some("Microsoft Windows [Version 10.0.26100]"));
        assert!(url.contains(
            "&os=windows&os_version=Microsoft+Windows+%5BVersion+10.0.26100%5D&state=paused&about="
        ));
        assert!(url.contains("%0A") && url.contains("%C2%B7"));
        assert!(!url.contains("&build="));
        let encoded = url.split("&about=").nth(1).unwrap();
        let decoded = form_decode(encoded);
        assert_eq!(decoded, dump.sync.about_block);
        let later_journal_line = render_journal_line(
            Some("1.2.3"),
            Some(&facts),
            false,
            Some(1_000),
            1_000 + 3 * 86_400,
        );
        assert_ne!(later_journal_line, frozen_journal_line);
        let later_url = build_report_url(&dump, Some("Microsoft Windows [Version 10.0.26100]"));
        assert_eq!(
            form_decode(later_url.split("&about=").nth(1).unwrap()),
            frozen_about_block
        );
        for forbidden in [
            "hostname",
            "DEVICE",
            "C:/private",
            "account-17",
            "192.0.2.1",
            "Model X",
        ] {
            assert!(!decoded.contains(forbidden));
        }
    }

    fn form_decode(encoded: &str) -> String {
        let bytes = encoded.as_bytes();
        let mut decoded = Vec::new();
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'+' => {
                    decoded.push(b' ');
                    index += 1;
                }
                b'%' if index + 2 < bytes.len() => {
                    let hex = std::str::from_utf8(&bytes[index + 1..index + 3]).unwrap();
                    decoded.push(u8::from_str_radix(hex, 16).unwrap());
                    index += 3;
                }
                byte => {
                    decoded.push(byte);
                    index += 1;
                }
            }
        }
        String::from_utf8(decoded).unwrap()
    }
}
