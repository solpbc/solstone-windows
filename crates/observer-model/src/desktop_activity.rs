// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Desktop activity source models, JSONL serialization, and URL normalization.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Coarse activity state reported by the activity hook / idle timer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActivityState {
    Active,
    Idle,
    Locked,
}

impl ActivityState {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Idle => "idle",
            Self::Locked => "locked",
        }
    }
}

/// Capabilities reported in availability rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityCapability {
    Foreground,
    Title,
    Url,
    Idle,
}

impl AvailabilityCapability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Foreground => "foreground",
            Self::Title => "title",
            Self::Url => "url",
            Self::Idle => "idle",
        }
    }
}

/// Status of a capability reported in an availability row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AvailabilityStatus {
    Unavailable,
    Denied,
    Unsupported,
}

impl AvailabilityStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Unavailable => "unavailable",
            Self::Denied => "denied",
            Self::Unsupported => "unsupported",
        }
    }
}

/// Payload for a `foreground` activity row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ForegroundPayload {
    pub app: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub app_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Payload for an `activity` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivityPayload {
    pub state: String,
}

/// Payload for an `availability` row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AvailabilityPayload {
    pub capability: String,
    pub status: String,
}

/// Inner payload tagged by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DesktopActivityPayload {
    Foreground(ForegroundPayload),
    Activity(ActivityPayload),
    Availability(AvailabilityPayload),
}

/// One line in `{segment}_{artifact-id}_desktop_activity.jsonl`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DesktopActivityRow {
    pub v: u32,
    pub t: String,
    pub kind: String,
    pub method: String,
    pub ts: u64,
    pub end_ts: u64,
    #[serde(flatten)]
    pub payload: DesktopActivityPayload,
}

impl DesktopActivityRow {
    pub fn foreground(
        ts: u64,
        end_ts: u64,
        app: String,
        app_id: Option<String>,
        title: Option<String>,
        url: Option<String>,
    ) -> Self {
        Self {
            v: 1,
            t: "desktop_activity".into(),
            kind: "foreground".into(),
            method: "winevent".into(),
            ts,
            end_ts,
            payload: DesktopActivityPayload::Foreground(ForegroundPayload {
                app,
                app_id,
                title,
                url,
            }),
        }
    }

    pub fn activity(ts: u64, end_ts: u64, state: ActivityState) -> Self {
        Self {
            v: 1,
            t: "desktop_activity".into(),
            kind: "activity".into(),
            method: "winevent".into(),
            ts,
            end_ts,
            payload: DesktopActivityPayload::Activity(ActivityPayload {
                state: state.as_str().into(),
            }),
        }
    }

    pub fn availability(
        ts: u64,
        end_ts: u64,
        method: &str,
        capability: AvailabilityCapability,
        status: AvailabilityStatus,
    ) -> Self {
        Self {
            v: 1,
            t: "desktop_activity".into(),
            kind: "availability".into(),
            method: method.into(),
            ts,
            end_ts,
            payload: DesktopActivityPayload::Availability(AvailabilityPayload {
                capability: capability.as_str().into(),
                status: status.as_str().into(),
            }),
        }
    }

    pub fn to_jsonl_line(&self) -> Result<String, serde_json::Error> {
        let mut s = serde_json::to_string(self)?;
        s.push('\n');
        Ok(s)
    }
}

/// Events emitted by the platform desktop-activity probe to the capture engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesktopActivityProbeEvent {
    Foreground {
        window_id: u64,
        app: String,
        exe_name: String,
        title: String,
    },
    TitleChanged {
        window_id: u64,
        title: String,
    },
    ActivityStateChanged(ActivityState),
    ForegroundUnavailable,
    IdleUnavailable,
}

/// Internal admission token; the window title itself never enters child arguments.
pub fn title_fingerprint(title: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    title.hash(&mut hasher);
    hasher.finish()
}

/// URL request sent to the probe's URL helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopActivityUrlRequest {
    pub generation: u64,
    pub window_id: u64,
    pub exe_name: String,
    pub title_fingerprint: u64,
}

/// URL result returned by the probe's URL helper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopActivityUrlResult {
    pub generation: u64,
    pub window_id: u64,
    pub url: Option<String>,
    pub unavailable: bool,
}

/// Current foreground window snapshot at admit time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ForegroundSnapshot {
    pub window_id: u64,
    pub app: String,
    pub exe_name: String,
    pub title: String,
}

/// Trait for the desktop activity capture source.
pub trait DesktopActivitySource: Send {
    fn start(&mut self) -> Result<(), crate::SourceError>;
    fn stop(&mut self);
    fn state(&self) -> crate::SourceState;
    fn poll_events(&mut self) -> Vec<DesktopActivityProbeEvent>;
    /// Content-free idle/session facts, without reading foreground identity/title.
    fn poll_coarse_events(&mut self) -> Vec<DesktopActivityProbeEvent> {
        Vec::new()
    }
    fn request_url(&mut self, request: DesktopActivityUrlRequest);
    fn poll_url_result(&mut self) -> Option<DesktopActivityUrlResult>;
    fn cancel_url(&mut self);
    fn foreground_snapshot(&mut self) -> Option<ForegroundSnapshot>;
}

fn is_legitimate_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    if host.parse::<std::net::Ipv4Addr>().is_ok() {
        return true;
    }
    if host.starts_with('[') && host.ends_with(']') {
        let inner = &host[1..host.len() - 1];
        if inner.parse::<std::net::Ipv6Addr>().is_ok() {
            return true;
        }
    }
    if !host.contains('.') || host.starts_with('.') || host.ends_with('.') {
        return false;
    }
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 {
            return false;
        }
        if label.starts_with('-') || label.ends_with('-') {
            return false;
        }
        if !label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
            return false;
        }
    }
    true
}

/// Normalize display or web URLs: keep explicit http(s) scheme or keep scheme-free display form;
/// keep host, port, path; strip userinfo, query, fragment; reject non-web schemes and malformed inputs.
/// Never logs raw input or formats it into errors.
pub fn normalize_url(raw: &str) -> Option<String> {
    // 1. Trim. Empty is None.
    let s = raw.trim();
    if s.is_empty() {
        return None;
    }

    // 2. Explicit http:// or https:// (ASCII case-insensitive): url::Url::parse the original.
    // Any other scheme:// is None.
    #[derive(Clone, Copy)]
    enum ExplicitScheme {
        Http,
        Https,
    }

    let explicit_scheme = if s
        .get(..7)
        .is_some_and(|p| p.eq_ignore_ascii_case("http://"))
    {
        Some(ExplicitScheme::Http)
    } else if s
        .get(..8)
        .is_some_and(|p| p.eq_ignore_ascii_case("https://"))
    {
        Some(ExplicitScheme::Https)
    } else {
        if s.contains("://") {
            return None;
        }
        None
    };

    let (parsed, had_slash) = if let Some(scheme) = explicit_scheme {
        let parsed = url::Url::parse(s).ok()?;
        let rest = match scheme {
            ExplicitScheme::Http => s.get(7..)?,
            ExplicitScheme::Https => s.get(8..)?,
        };
        let before_query_frag = rest.split_once(['?', '#']).map(|(b, _)| b).unwrap_or(rest);
        let had_slash = before_query_frag.contains('/');
        (parsed, had_slash)
    } else {
        // 3. Otherwise, if the string matches a scheme token ^[A-Za-z][A-Za-z0-9+.-]*:
        // and the characters after that colon, up to /, ?, or #, are not an all-digit port,
        // return None.
        if let Some(colon_pos) = s.find(':') {
            let before_colon = &s[..colon_pos];
            let is_scheme_token = !before_colon.is_empty()
                && before_colon.chars().next().unwrap().is_ascii_alphabetic()
                && before_colon
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '+' || c == '.' || c == '-');

            if is_scheme_token {
                let after_colon = &s[colon_pos + 1..];
                let up_to_delimiter = after_colon
                    .split_once(['/', '?', '#'])
                    .map(|(b, _)| b)
                    .unwrap_or(after_colon);

                let is_all_digit_port = !up_to_delimiter.is_empty()
                    && up_to_delimiter.chars().all(|c| c.is_ascii_digit());

                if !is_all_digit_port {
                    return None;
                }
            }
        }

        // 4. Display form: prefix https:// only when the host is legitimate, then parse with url::Url.
        // Legitimate host means localhost, an IPv4 address, a bracketed IPv6 literal,
        // or a DNS name with at least one dot and normal labels. Bare words (example, not a url) are None.
        let before_query_frag = s.split_once(['?', '#']).map(|(b, _)| b).unwrap_or(s);
        let had_slash = before_query_frag.contains('/');
        let authority = before_query_frag.split('/').next().unwrap_or("");
        if authority.is_empty() {
            return None;
        }

        let host = if authority.starts_with('[') {
            let end_bracket = authority.find(']')?;
            let h = &authority[..=end_bracket];
            let after = &authority[end_bracket + 1..];
            if !after.is_empty()
                && (!after.starts_with(':') || !after[1..].chars().all(|c| c.is_ascii_digit()))
            {
                return None;
            }
            h
        } else {
            match authority.split_once(':') {
                Some((h, port)) => {
                    if port.is_empty() || !port.chars().all(|c| c.is_ascii_digit()) {
                        return None;
                    }
                    h
                }
                None => authority,
            }
        };

        if !is_legitimate_host(host) {
            return None;
        }

        let display_url = format!("https://{}", s);
        let parsed = url::Url::parse(&display_url).ok()?;
        (parsed, had_slash)
    };

    // 5. Parsed scheme must be http or https.
    let scheme = parsed.scheme();
    if scheme != "http" && scheme != "https" {
        return None;
    }

    let host_str = parsed.host_str()?;

    let mut out = String::new();
    if explicit_scheme.is_some() {
        out.push_str(scheme);
        out.push_str("://");
    }
    out.push_str(host_str);
    if let Some(port) = parsed.port() {
        out.push(':');
        out.push_str(&port.to_string());
    }
    let path = parsed.path();
    if path == "/" && !had_slash {
        // omit path that is only / when input had no slash
    } else {
        out.push_str(path);
    }

    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_normalize_display_url() {
        assert_eq!(
            normalize_url("example.com/path"),
            Some("example.com/path".to_string())
        );
        assert_eq!(
            normalize_url("127.0.0.1:48125/path?q=secret#frag"),
            Some("127.0.0.1:48125/path".to_string())
        );
    }

    #[test]
    fn test_normalize_explicit_scheme() {
        assert_eq!(
            normalize_url("http://example.com/path"),
            Some("http://example.com/path".to_string())
        );
        assert_eq!(
            normalize_url("https://example.com:8443/secure?foo=bar#top"),
            Some("https://example.com:8443/secure".to_string())
        );
        assert_eq!(
            normalize_url("https://user:password@example.com/path?q=1#f"),
            Some("https://example.com/path".to_string())
        );
    }

    #[test]
    fn test_normalize_credentials_stripped() {
        assert_eq!(
            normalize_url("https://user:password@example.com/path"),
            Some("https://example.com/path".to_string())
        );
    }

    #[test]
    fn test_normalize_rejections() {
        assert!(normalize_url("mailto:user@example.com/path").is_none());
        assert!(normalize_url("javascript:foo@example.com/path").is_none());
        assert!(normalize_url("javascript:alert(1)").is_none());
        assert!(normalize_url("file://").is_none());
        assert!(normalize_url("file:///C:/secrets.txt").is_none());
        assert!(normalize_url("data:").is_none());
        assert!(normalize_url("data:text/html,hello").is_none());
        assert!(normalize_url("chrome:").is_none());
        assert!(normalize_url("chrome://settings").is_none());
        assert!(normalize_url("about:").is_none());
        assert!(normalize_url("about:blank").is_none());
        assert!(normalize_url("ftp://").is_none());
        assert!(normalize_url("example").is_none());
        assert!(normalize_url("not a url").is_none());
        assert!(normalize_url("https://").is_none());
        assert!(normalize_url("").is_none());
    }

    #[test]
    fn test_row_jsonl_serialization() {
        let fg = DesktopActivityRow::foreground(
            1000,
            2000,
            "Code".into(),
            None,
            Some("lib.rs".into()),
            Some("example.com/path".into()),
        );
        let line = fg.to_jsonl_line().expect("serialize");
        assert!(line.ends_with('\n'));
        assert!(line.contains("\"v\":1"));
        assert!(line.contains("\"t\":\"desktop_activity\""));
        assert!(line.contains("\"kind\":\"foreground\""));
        assert!(line.contains("\"method\":\"winevent\""));
        assert!(line.contains("\"app\":\"Code\""));
        assert!(line.contains("\"title\":\"lib.rs\""));
        assert!(line.contains("\"url\":\"example.com/path\""));

        let act = DesktopActivityRow::activity(2000, 3000, ActivityState::Active);
        let line = act.to_jsonl_line().expect("serialize");
        assert!(line.contains("\"state\":\"active\""));

        let avail = DesktopActivityRow::availability(
            3000,
            4000,
            "uia",
            AvailabilityCapability::Url,
            AvailabilityStatus::Unavailable,
        );
        assert!(
            avail.payload
                == DesktopActivityPayload::Availability(AvailabilityPayload {
                    capability: "url".into(),
                    status: "unavailable".into(),
                })
        );
    }

    #[test]
    fn test_normalize_utf8_char_boundary_and_non_ascii_path() {
        let non_boundary = "éééé";
        assert!(
            normalize_url(non_boundary).is_none(),
            "expected None for non-boundary UTF-8 input"
        );

        let non_ascii_url = "https://example.com/café?query=1#frag";
        let normalized = normalize_url(non_ascii_url);
        assert_eq!(
            normalized,
            Some("https://example.com/caf%C3%A9".to_string())
        );
    }
}
