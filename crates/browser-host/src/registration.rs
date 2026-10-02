// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! What to register so Chrome, Edge and Firefox can start the host.
//!
//! Every row is rendered by the shared contract crate from its one
//! registration table; this only chooses the rows and names the manifest
//! files. Writing them (HKCU, both registry views) is `platform-win`'s.

use native_browser_frame::render_registration;

/// One browser's registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    pub channel: &'static str,
    pub browser: &'static str,
    /// The `HKEY_CURRENT_USER` subkey whose default value names the manifest.
    pub registry_key: String,
    /// The manifest's file name in the app's registration directory.
    pub manifest_file: String,
    /// The manifest's contents.
    pub json: String,
}

const BROWSERS: [&str; 3] = ["chrome", "edge", "firefox"];

fn rows_for(channel: &'static str, exe: &str) -> Vec<Row> {
    BROWSERS
        .iter()
        .filter_map(|browser| {
            let r = render_registration(channel, browser, "windows", Some(exe), None).ok()?;
            Some(Row {
                channel,
                browser,
                manifest_file: format!("{}.{}.json", r.host, browser),
                registry_key: r.path,
                json: r.json,
            })
        })
        .collect()
}

/// The rows this build registers: production always, development only in a
/// build that admits the development ids.
pub fn wanted(exe: &str, development: bool) -> Vec<Row> {
    let mut rows = rows_for("production", exe);
    if development {
        rows.extend(rows_for("dev", exe));
    }
    rows
}

/// Rows this build must not leave behind: the development rows, in a build
/// without the development host (a preview installed over, then replaced).
pub fn unwanted(exe: &str, development: bool) -> Vec<Row> {
    if development {
        Vec::new()
    } else {
        rows_for("dev", exe)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXE: &str = r"C:\Users\Zoë Q\AppData\Local\Solstone\current\solstone-windows-app.exe";

    #[test]
    fn production_registers_three_browsers_with_production_ids_only() {
        let rows = wanted(EXE, false);
        assert_eq!(rows.len(), 3);
        let keys: Vec<&str> = rows.iter().map(|r| r.registry_key.as_str()).collect();
        assert_eq!(
            keys,
            [
                r"Software\Google\Chrome\NativeMessagingHosts\app.solstone.browser",
                r"Software\Microsoft\Edge\NativeMessagingHosts\app.solstone.browser",
                r"Software\Mozilla\NativeMessagingHosts\app.solstone.browser",
            ]
        );
        for row in &rows {
            let v: serde_json::Value = serde_json::from_str(&row.json).unwrap();
            assert_eq!(v["path"], EXE);
            assert!(!row.json.contains("fgfnkcefedeheoeamppkiiloncfekakf"));
            assert!(!row.json.contains("browser.dev@solstone.app"));
        }
        assert!(rows[0]
            .json
            .contains("chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim/"));
        assert!(rows[2].json.contains("browser@solstone.app"));
        assert_eq!(rows[2].manifest_file, "app.solstone.browser.firefox.json");
        assert_eq!(unwanted(EXE, false).len(), 3);
    }

    #[test]
    fn a_development_build_adds_the_dev_host_rows() {
        let rows = wanted(EXE, true);
        assert_eq!(rows.len(), 6);
        assert!(rows
            .iter()
            .any(|r| r.registry_key.ends_with(r"\app.solstone.browser.dev")));
        assert!(unwanted(EXE, true).is_empty());
    }
}
