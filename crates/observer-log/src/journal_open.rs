// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Classifiers and a URL masker for diagnosing a silent journal-window open
//! failure. Pure helpers over primitives only - no platform, tauri, or url
//! dependency. The GUI passes in already-extracted primitives.

/// Which failure mode explains a failed journal-window open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JournalOpenFailure {
    NavNeverStarted,
    LoadHung,
    NotUsable,
}

impl JournalOpenFailure {
    pub fn token(&self) -> &'static str {
        match self {
            Self::NavNeverStarted => "nav_never_started",
            Self::LoadHung => "load_hung",
            Self::NotUsable => "not_usable",
        }
    }
}

/// Classify a failed open. Only called on the failure domain (`!navigated ||
/// !usable`).
///
/// Rules:
/// - `navigated == true` (so, in the failure domain, `usable == false`) -> `NotUsable`.
/// - `navigated == false` -> `LoadHung` iff `page_load_started || bridge_contacted`,
///   else `NavNeverStarted`.
///
/// Total function: the `(navigated, usable) == (true, true)` success input is
/// never classified; it is debug-asserted against and falls through to
/// `NotUsable`.
pub fn classify_journal_open_failure(
    navigated: bool,
    page_load_started: bool,
    bridge_contacted: bool,
    usable: bool,
) -> JournalOpenFailure {
    if navigated {
        debug_assert!(
            !usable,
            "classify_journal_open_failure called on the success input"
        );
        JournalOpenFailure::NotUsable
    } else if page_load_started || bridge_contacted {
        JournalOpenFailure::LoadHung
    } else {
        JournalOpenFailure::NavNeverStarted
    }
}

/// Why a window failed the usability check, or `None` if it is usable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsableFailureReason {
    QueryError,
    NotVisible,
    Minimized,
    ZeroInner,
    ZeroOuter,
}

impl UsableFailureReason {
    pub fn token(&self) -> &'static str {
        match self {
            Self::QueryError => "query_error",
            Self::NotVisible => "not_visible",
            Self::Minimized => "minimized",
            Self::ZeroInner => "zero_inner",
            Self::ZeroOuter => "zero_outer",
        }
    }
}

/// First failing usability predicate over already-queried primitives, or `None`
/// if the window is usable. A `None` input means the underlying query errored
/// (`QueryError`). Predicate order mirrors the GUI's original bool check:
/// visible, then not-minimized, then non-zero inner, then non-zero outer.
pub fn usable_failure_reason(
    visible: Option<bool>,
    minimized: Option<bool>,
    inner: Option<(u32, u32)>,
    outer: Option<(u32, u32)>,
) -> Option<UsableFailureReason> {
    match visible {
        None => return Some(UsableFailureReason::QueryError),
        Some(false) => return Some(UsableFailureReason::NotVisible),
        Some(true) => {}
    }
    match minimized {
        None => return Some(UsableFailureReason::QueryError),
        Some(true) => return Some(UsableFailureReason::Minimized),
        Some(false) => {}
    }
    match inner {
        None => return Some(UsableFailureReason::QueryError),
        Some((w, h)) if w == 0 || h == 0 => return Some(UsableFailureReason::ZeroInner),
        Some(_) => {}
    }
    match outer {
        None => return Some(UsableFailureReason::QueryError),
        Some((w, h)) if w == 0 || h == 0 => return Some(UsableFailureReason::ZeroOuter),
        Some(_) => {}
    }
    None
}

/// Mask the `cap` query value in a bridge URL, preserving scheme, host, port,
/// path, and every other query parameter - those are the diagnostic signal.
/// A URL with no query, or no `cap` parameter, is returned unchanged.
pub fn strip_cap(url: &str) -> String {
    let Some((prefix, query)) = url.split_once('?') else {
        return url.to_string();
    };
    let masked = query
        .split('&')
        .map(|param| match param.split_once('=') {
            Some(("cap", _)) => "cap=<redacted>".to_string(),
            _ => param.to_string(),
        })
        .collect::<Vec<_>>()
        .join("&");
    format!("{prefix}?{masked}")
}

#[cfg(test)]
mod tests {
    use super::*;

    // classify: full truth table over the failure domain (!navigated || !usable).
    #[test]
    fn classify_navigated_but_not_usable_is_not_usable() {
        // navigated=true implies usable=false in the failure domain.
        assert_eq!(
            classify_journal_open_failure(true, false, false, false),
            JournalOpenFailure::NotUsable
        );
        assert_eq!(
            classify_journal_open_failure(true, true, true, false),
            JournalOpenFailure::NotUsable
        );
    }

    #[test]
    fn classify_not_navigated_no_progress_is_nav_never_started() {
        assert_eq!(
            classify_journal_open_failure(false, false, false, false),
            JournalOpenFailure::NavNeverStarted
        );
        // Still nav_never_started even when window happens to be usable.
        assert_eq!(
            classify_journal_open_failure(false, false, false, true),
            JournalOpenFailure::NavNeverStarted
        );
    }

    #[test]
    fn classify_not_navigated_with_progress_is_load_hung() {
        assert_eq!(
            classify_journal_open_failure(false, true, false, false),
            JournalOpenFailure::LoadHung
        );
        assert_eq!(
            classify_journal_open_failure(false, false, true, false),
            JournalOpenFailure::LoadHung
        );
        assert_eq!(
            classify_journal_open_failure(false, true, true, false),
            JournalOpenFailure::LoadHung
        );
    }

    #[test]
    fn usable_all_good_is_none() {
        assert_eq!(
            usable_failure_reason(
                Some(true),
                Some(false),
                Some((1100, 800)),
                Some((1120, 840))
            ),
            None
        );
    }

    #[test]
    fn usable_reports_first_failing_predicate() {
        assert_eq!(
            usable_failure_reason(
                Some(false),
                Some(false),
                Some((1100, 800)),
                Some((1120, 840))
            ),
            Some(UsableFailureReason::NotVisible)
        );
        assert_eq!(
            usable_failure_reason(Some(true), Some(true), Some((1100, 800)), Some((1120, 840))),
            Some(UsableFailureReason::Minimized)
        );
        assert_eq!(
            usable_failure_reason(Some(true), Some(false), Some((0, 800)), Some((1120, 840))),
            Some(UsableFailureReason::ZeroInner)
        );
        assert_eq!(
            usable_failure_reason(Some(true), Some(false), Some((1100, 800)), Some((1120, 0))),
            Some(UsableFailureReason::ZeroOuter)
        );
    }

    #[test]
    fn usable_query_error_for_any_none_input() {
        assert_eq!(
            usable_failure_reason(None, Some(false), Some((1100, 800)), Some((1120, 840))),
            Some(UsableFailureReason::QueryError)
        );
        assert_eq!(
            usable_failure_reason(Some(true), None, Some((1100, 800)), Some((1120, 840))),
            Some(UsableFailureReason::QueryError)
        );
        assert_eq!(
            usable_failure_reason(Some(true), Some(false), None, Some((1120, 840))),
            Some(UsableFailureReason::QueryError)
        );
        assert_eq!(
            usable_failure_reason(Some(true), Some(false), Some((1100, 800)), None),
            Some(UsableFailureReason::QueryError)
        );
    }

    #[test]
    fn strip_cap_masks_only_the_cap_value() {
        let out = strip_cap("http://127.0.0.1:1234/_bridge/bootstrap?cap=deadbeefsecret");
        assert!(out.contains("http://127.0.0.1:1234/_bridge/bootstrap"));
        assert!(out.contains("cap=<redacted>"));
        assert!(!out.contains("deadbeefsecret"));
    }

    #[test]
    fn strip_cap_preserves_other_params_and_position() {
        assert_eq!(
            strip_cap("http://127.0.0.1:1234/foo?a=1&cap=secret&b=2"),
            "http://127.0.0.1:1234/foo?a=1&cap=<redacted>&b=2"
        );
    }

    #[test]
    fn strip_cap_unchanged_without_query_or_cap() {
        assert_eq!(
            strip_cap("http://127.0.0.1:1234/"),
            "http://127.0.0.1:1234/"
        );
        assert_eq!(
            strip_cap("http://127.0.0.1:1234/foo?a=1&b=2"),
            "http://127.0.0.1:1234/foo?a=1&b=2"
        );
    }
}
