// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use observer_model::{HealthDump, SourceState};
use std::process::Command;

pub const HELP_URL: &str = "https://support.solstone.app";

pub fn report_url(dump: &HealthDump) -> String {
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
    let build = option_env!("SOLSTONE_SOURCE_COMMIT").unwrap_or("development");
    if !build.is_empty() {
        fields.push(("build", build.chars().take(120).collect()));
    }
    fields.push(("os", "windows".to_owned()));
    if let Some(os_version) = windows_version() {
        fields.push(("os_version", os_version));
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
    fragment_url(fields)
}

fn source_error_code(source: &observer_model::SourceReport) -> Option<&'static str> {
    match &source.state {
        SourceState::Faulted { reason, .. } => Some((*reason).into()),
        _ => None,
    }
}

fn recent_state_lines(dump: &HealthDump) -> String {
    let mut lines = dump
        .sources
        .iter()
        .map(|source| {
            let kind: &'static str = source.kind.into();
            let state = match &source.state {
                SourceState::Active => "active",
                SourceState::Inactive => "inactive",
                SourceState::NoInputDevice => "no_input_device",
                SourceState::Faulted { reason, .. } => (*reason).into(),
            };
            format!("{kind}: {state}")
        })
        .collect::<Vec<_>>();
    if let Some(code) = dump.sync.upload.last_error_reason.as_deref() {
        lines.push(format!("sync: {code}"));
    }
    lines.join("\n")
}

fn windows_version() -> Option<String> {
    Command::new("cmd")
        .args(["/C", "ver"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|value| value.trim().chars().take(120).collect())
        .filter(|value: &String| !value.is_empty())
}

fn fragment_url(fields: Vec<(&str, String)>) -> String {
    let fragment = fields
        .into_iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(&value)))
        .collect::<Vec<_>>()
        .join("&");
    format!("{HELP_URL}/#{fragment}")
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
    use observer_model::{AppPhase, SyncSnapshot};
    use std::collections::BTreeMap;

    #[test]
    fn encoding_matches_url_search_params() {
        assert_eq!(form_encode("a b+c~\n"), "a+b%2Bc%7E%0A");
    }

    #[test]
    fn report_uses_the_fixed_fragment_contract() {
        let dump = HealthDump {
            app_state: AppPhase::Paused,
            sources: Vec::new(),
            frame_rate: None,
            segment_dir: None,
            segment_seconds_remaining: None,
            engine_ready: true,
            version: "2.0.0".to_owned(),
            sync: SyncSnapshot::default(),
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: BTreeMap::new(),
            pump_degraded: false,
        };
        let url = report_url(&dump);
        assert!(url.starts_with("https://support.solstone.app/#report=v1&app=solstone+for+windows"));
        assert!(url.contains("&state=paused"));
        assert!(!url.contains('?'));
        assert!(!url.contains("segment"));
        assert!(!url.contains("journal"));
    }
}
