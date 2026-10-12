// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BrowserStore {
    pub brand: &'static str,
    pub url: &'static str,
    pub executable: &'static str,
}

/// One Chrome Web Store item. Edge installs that same item.
const STORE_URL_PREFIX: &str = "https://chromewebstore.google.com/detail/solstone/";

fn store_url() -> &'static str {
    static URL: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    URL.get_or_init(|| format!("{STORE_URL_PREFIX}{}", crate::frame::PROD_CHROME_ID))
        .as_str()
}

pub fn stores() -> [BrowserStore; 2] {
    let url = store_url();
    [
        BrowserStore {
            brand: "chrome",
            url,
            executable: "chrome.exe",
        },
        BrowserStore {
            brand: "edge",
            url,
            executable: "msedge.exe",
        },
    ]
}

pub fn get(brand: &str) -> Option<BrowserStore> {
    stores().into_iter().find(|entry| entry.brand == brand)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowsersTray {
    Names(Vec<String>),
    Paused,
    Off,
    None,
}

/// Connected brands win, in the order given, with duplicates collapsed.
/// Pause and intake-off apply only when nothing is connected.
pub fn browsers_tray(capture: &str, connected_brands_in_order: &[&str]) -> BrowsersTray {
    let mut names = Vec::new();
    for brand in connected_brands_in_order {
        if !names.iter().any(|seen: &String| seen == brand) {
            names.push((*brand).to_string());
        }
    }
    if !names.is_empty() {
        return BrowsersTray::Names(names);
    }
    match capture {
        "paused" => BrowsersTray::Paused,
        "intake_off" => BrowsersTray::Off,
        _ => BrowsersTray::None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn get_chrome_and_edge_share_production_store_url() {
        let expected_url =
            "https://chromewebstore.google.com/detail/solstone/eibbeeoifjoabddfmgeggnageolkcnim";
        let formatted_url = format!(
            "https://chromewebstore.google.com/detail/solstone/{}",
            crate::frame::PROD_CHROME_ID
        );
        assert_eq!(expected_url, formatted_url);

        let chrome = get("chrome").expect("chrome store");
        assert_eq!(chrome.url, expected_url);
        assert_eq!(chrome.url, formatted_url);
        assert_eq!(chrome.executable, "chrome.exe");

        let edge = get("edge").expect("edge store");
        assert_eq!(edge.url, expected_url);
        assert_eq!(edge.url, formatted_url);
        assert_eq!(edge.executable, "msedge.exe");
    }

    #[test]
    fn get_returns_none_for_missing_or_empty_brands() {
        assert!(get("firefox").is_none());
        assert!(get("unknown").is_none());
        assert!(get("").is_none());
    }

    #[test]
    fn brand_order_of_stores() {
        let brands: Vec<&str> = stores().into_iter().map(|s| s.brand).collect();
        assert_eq!(brands, ["chrome", "edge"]);
    }

    #[test]
    fn browsers_tray_connected_brands_order_and_deduplication() {
        assert_eq!(
            browsers_tray("paused", &["chrome", "edge"]),
            BrowsersTray::Names(vec!["chrome".to_string(), "edge".to_string()])
        );
        assert_eq!(
            browsers_tray("paused", &["chrome", "chrome", "edge"]),
            BrowsersTray::Names(vec!["chrome".to_string(), "edge".to_string()])
        );
        assert_eq!(
            browsers_tray("intake_off", &["edge", "chrome"]),
            BrowsersTray::Names(vec!["edge".to_string(), "chrome".to_string()])
        );
    }

    #[test]
    fn browsers_tray_empty_connected_states() {
        assert_eq!(browsers_tray("paused", &[]), BrowsersTray::Paused);
        assert_eq!(browsers_tray("intake_off", &[]), BrowsersTray::Off);
        assert_eq!(browsers_tray("not_paired", &[]), BrowsersTray::None);
        assert_eq!(browsers_tray("unavailable", &[]), BrowsersTray::None);
        assert_eq!(browsers_tray("permitted", &[]), BrowsersTray::None);
        assert_eq!(browsers_tray("something-else", &[]), BrowsersTray::None);
    }
}
