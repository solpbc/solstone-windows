// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use observer_model::HealthDump;
use std::process::Command;

pub const HELP_URL: &str = "https://support.solstone.app";

pub fn report_url(dump: &HealthDump) -> String {
    let os_version = windows_version();
    observer_model::about::build_report_url(dump, os_version.as_deref())
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

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::{AppPhase, SyncSnapshot};
    use std::collections::BTreeMap;

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
            sync: SyncSnapshot {
                about_app_line: "windows app 2.0.0 · windows 11 26100 · x86_64".into(),
                journal_display_line: "journal 1.2.3 · ubuntu 24.04 · x86_64".into(),
                about_block: "windows app 2.0.0 · windows 11 26100 · x86_64\njournal 1.2.3 · ubuntu 24.04 · x86_64".into(),
                ..SyncSnapshot::default()
            },
            screen_encoder: None,
            exclusions: None,
            storage: None,
            pause: None,
            views: BTreeMap::new(),
            pump_degraded: false,
            listener_faults: observer_model::ListenerFaults::default(),
        };
        let url = report_url(&dump);
        assert!(url.starts_with("https://support.solstone.app/#report=v1&app=solstone+for+windows"));
        assert!(url.contains("&state=paused"));
        assert!(!url.contains('?'));
        assert!(!url.contains("segment"));
        assert!(!url.contains("&build="));
        assert!(url.contains(
            "&about=windows+app+2.0.0+%C2%B7+windows+11+26100+%C2%B7+x86_64%0Ajournal+1.2.3"
        ));
        for private in [
            "PRIVATE HOST",
            "DEVICE",
            "C:/private",
            "account-17",
            "192.0.2.1",
            "Model X",
        ] {
            assert!(!url.contains(private));
        }
    }
}
