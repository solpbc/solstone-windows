// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Capture-exclusion policy — the trust-critical core.
//!
//! This is the **pure tier**: no platform dependency, no `unsafe`, fully
//! host-testable. It owns three things:
//!
//! 1. [`ExclusionRules`] — the owner-configured policy (excluded apps,
//!    window-title patterns, auto-exclude private-browsing), persisted to
//!    `exclusions.json` and edited over IPC.
//! 2. [`evaluate`] — given the rules and the windows present on the captured
//!    display *at frame time* (enumerated by the platform tier), decides whether
//!    the frame passes through, has regions redacted, or must be dropped whole.
//! 3. [`apply_redaction`] — blacks out the excluded regions in a frame buffer.
//!
//! **Why software redaction.** Unlike macOS (ScreenCaptureKit's
//! `SCContentFilter` excludes windows structurally at the compositor), Windows
//! WGC has no per-window exclude on a monitor capture, and
//! `SetWindowDisplayAffinity` only governs a process's *own* windows. So the
//! excluded surface lands in the composited monitor frame and we remove it here,
//! post-capture, before the frame is encoded into a segment.
//!
//! **Fail closed, never silently.** The guarantee is that an excluded surface
//! never enters a segment. When the platform tier can identify an excluded
//! window but cannot give us reliable geometry to redact it — or cannot read a
//! window's identity at all while a content-dependent rule is active — the only
//! honest move is to drop the whole frame ([`ExclusionDecision::Drop`]) rather
//! than risk a leak. The caller surfaces drop/redaction counts in health so the
//! owner sees that exclusion is doing its job.

#![forbid(unsafe_code)]

use observer_model::ScreenPixelFormat;
use serde::{Deserialize, Serialize};

fn default_true() -> bool {
    true
}

/// Owner-configured capture-exclusion policy. Persisted to `exclusions.json` and
/// surfaced/edited via the IPC command surface. The default excludes nothing by
/// app/title but auto-excludes private-browsing (matching the macOS observer's
/// default), the trust-forward choice: a fresh install errs toward *not*
/// capturing private windows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExclusionRules {
    /// Executable file names (e.g. `"slack.exe"`), matched **case-insensitively
    /// and exactly** against a window's owning process image name — never a
    /// substring. This is the robust-process-identity key that avoids the macOS
    /// audit's fuzzy-name failure mode: the owner picks from the live running-app
    /// list, so the value is always a real running process's exe.
    #[serde(default)]
    pub excluded_exes: Vec<String>,
    /// Window-title keywords. A window is excluded when its title **contains**
    /// any of these (case-insensitive substring), matching the macOS
    /// title-pattern model. Plain substrings, not glob/regex.
    #[serde(default)]
    pub title_patterns: Vec<String>,
    /// Auto-exclude private/incognito browser windows. Detection is a
    /// title-string heuristic keyed on the browser's exe — the same robustness
    /// the macOS observer ships (it, too, reads the window title, not an
    /// Accessibility API). Honest caveat for owner copy: catches the mainstream
    /// browsers in their default private modes; a browser that doesn't mark its
    /// title, or one not in the table, is not auto-detected.
    #[serde(default = "default_true")]
    pub exclude_private_browsing: bool,
}

impl Default for ExclusionRules {
    fn default() -> Self {
        Self {
            excluded_exes: Vec::new(),
            title_patterns: Vec::new(),
            exclude_private_browsing: true,
        }
    }
}

impl ExclusionRules {
    /// Whether any rule is configured. The capture hot path skips window
    /// enumeration entirely when this is false — no rules, no per-frame cost.
    pub fn is_active(&self) -> bool {
        !self.excluded_exes.is_empty()
            || !self.title_patterns.is_empty()
            || self.exclude_private_browsing
    }

    /// Canonical form for storage and matching: trimmed, exe names lowercased,
    /// title patterns lowercased, empties dropped, de-duplicated (order-stable).
    /// Applied when the owner sets rules so the stored form is the matched form.
    pub fn normalized(&self) -> Self {
        Self {
            excluded_exes: dedupe_lower(&self.excluded_exes),
            title_patterns: dedupe_lower(&self.title_patterns),
            exclude_private_browsing: self.exclude_private_browsing,
        }
    }
}

fn dedupe_lower(values: &[String]) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = v.trim().to_ascii_lowercase();
        if v.is_empty() || out.contains(&v) {
            continue;
        }
        out.push(v);
    }
    out
}

/// A rectangle in the captured monitor's physical-pixel space (origin at the
/// monitor's top-left). `x`/`y` may be negative when a window straddles the
/// monitor's top/left edge; [`apply_redaction`] clamps to the frame bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

/// One top-level window present on the captured display at frame time, as the
/// platform enumerator (`capture-wgc`) reports it. Geometry is already mapped
/// into the captured monitor's physical-pixel space. `Serialize` so the
/// `--dump-windows` diagnostic can emit the enumerator's view as JSON.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WindowInfo {
    /// Owning process image file name, lowercased (e.g. `"chrome.exe"`). Empty
    /// when it could not be read (see `identity_uncertain`).
    pub exe_name: String,
    /// Window title. Empty when the window has no title or it could not be read.
    pub title: String,
    /// Visible bounds in monitor-relative physical pixels, or `None` when the
    /// enumerator could not determine them with confidence (a failed geometry
    /// call, an un-mappable coordinate, a DPI mismatch). A `None` here on an
    /// *excluded* window forces a whole-frame drop — we will not encode a frame
    /// we can't redact safely.
    pub bounds: Option<Rect>,
    /// The enumerator could not read this window's identity (exe and/or title)
    /// with confidence — e.g. a higher-integrity process not queryable from the
    /// medium-integrity observer. With a content-dependent rule active, this
    /// forces a drop (we can't prove the window isn't an excluded surface).
    pub identity_uncertain: bool,
}

/// A distinct running application offered to the owner in the exclusion picker.
/// The owner picks one of these so the stored exclusion key is always a real
/// running process's exe — the robust-identity path that avoids guessable
/// free-text and the macOS fuzzy-name failure mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningApp {
    /// Lowercased exe file name (the value stored in [`ExclusionRules`]).
    pub exe_name: String,
    /// A human-friendly label (a representative window title), for display only.
    pub display_name: String,
}

/// The verdict for one captured frame. Adjacently tagged so the `Redact` rects
/// serialize (an internally-tagged enum cannot hold a newtype-of-sequence) — this
/// shape is only used by the `--dump-windows` diagnostic; the capture path matches
/// the value directly and never serializes it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "verdict", content = "rects", rename_all = "snake_case")]
pub enum ExclusionDecision {
    /// No excluded surface present — encode the frame unchanged.
    Pass,
    /// Black out these regions, then encode.
    Redact(Vec<Rect>),
    /// An excluded surface is present that we cannot safely redact (unknown
    /// geometry, or an unreadable window while a content rule is active). Drop
    /// the whole frame — fail closed, never leak.
    Drop,
}

/// Browser exe → the case-insensitive title markers that denote a private /
/// incognito window. Title-based, matching the macOS observer's robustness.
/// Verified on the build box; extend here as new browsers are confirmed.
fn private_markers(exe_name: &str) -> Option<&'static [&'static str]> {
    match exe_name {
        "chrome.exe" => Some(&["incognito"]),
        "msedge.exe" => Some(&["inprivate"]),
        "brave.exe" => Some(&["private", "tor"]),
        "firefox.exe" => Some(&["private browsing"]),
        _ => None,
    }
}

/// Whether a window is a private/incognito browser window, by the title
/// heuristic keyed on the browser exe. Public so the running-app picker and
/// tests can reuse the exact production logic.
pub fn is_private_window(exe_name: &str, title: &str) -> bool {
    let exe = exe_name.to_ascii_lowercase();
    let Some(markers) = private_markers(&exe) else {
        return false;
    };
    let title = title.to_ascii_lowercase();
    markers.iter().any(|marker| title.contains(marker))
}

fn exe_excluded(exe_name: &str, excluded: &[String]) -> bool {
    if exe_name.is_empty() {
        return false;
    }
    // Exact, case-insensitive — NOT substring. "slack.exe" must not match
    // "slackbot.exe". This is the anti-fuzzy-match invariant.
    excluded.iter().any(|x| x.eq_ignore_ascii_case(exe_name))
}

fn title_excluded(title: &str, patterns: &[String]) -> bool {
    if title.is_empty() || patterns.is_empty() {
        return false;
    }
    let title = title.to_ascii_lowercase();
    patterns
        .iter()
        .any(|p| !p.is_empty() && title.contains(&p.to_ascii_lowercase()))
}

fn window_excluded(rules: &ExclusionRules, window: &WindowInfo) -> bool {
    exe_excluded(&window.exe_name, &rules.excluded_exes)
        || title_excluded(&window.title, &rules.title_patterns)
        || (rules.exclude_private_browsing && is_private_window(&window.exe_name, &window.title))
}

/// Decide what to do with a captured frame given the rules and the windows
/// present on the display at frame time. The caller (the WGC source) applies the
/// verdict to the owned frame buffer before emitting it to the encoder.
///
/// Fail-closed order matters: an unreadable-identity real window (while any rule
/// is active) or a known-excluded window with no geometry both short-circuit to
/// [`ExclusionDecision::Drop`] before any redaction is assembled.
pub fn evaluate(rules: &ExclusionRules, windows: &[WindowInfo]) -> ExclusionDecision {
    if !rules.is_active() {
        return ExclusionDecision::Pass;
    }
    let mut rects: Vec<Rect> = Vec::new();
    for window in windows {
        if window.identity_uncertain {
            // A real on-screen surface whose identity the platform tier could not
            // read, while exclusion is active. We cannot prove it is *not* an
            // excluded surface — an excluded app could be running elevated under a
            // name we can't query. Fail closed; the drop is counted into health,
            // so the owner sees it rather than a silent leak. (The enumerator only
            // marks a window uncertain when it is a real, visible, on-monitor
            // surface — tool/cloaked/zero-area windows are filtered out, so a
            // normal desktop produces none of these.)
            return ExclusionDecision::Drop;
        }
        if window_excluded(rules, window) {
            match window.bounds {
                Some(rect) => rects.push(rect),
                // Known-excluded but un-redactable: drop rather than leak.
                None => return ExclusionDecision::Drop,
            }
        }
    }
    if rects.is_empty() {
        ExclusionDecision::Pass
    } else {
        ExclusionDecision::Redact(rects)
    }
}

/// Black out `rects` in a packed 4-byte-per-pixel frame buffer (RGBA8 or BGRA8).
/// Opaque black is `[0, 0, 0, 255]` in both layouts (R=G=B=0; alpha is the 4th
/// byte either way), so the pixel format does not change the bytes written.
/// Rectangles are clamped to the frame; out-of-bounds and zero-area rects are
/// no-ops. A buffer shorter than `width*height*4` is tolerated (per-pixel bounds
/// check) rather than panicking — validate-at-boundary, never crash capture.
pub fn apply_redaction(
    pixels: &mut [u8],
    width: u32,
    height: u32,
    _format: ScreenPixelFormat,
    rects: &[Rect],
) {
    if width == 0 || height == 0 || rects.is_empty() {
        return;
    }
    let stride = width as usize * 4;
    let frame_w = width as i64;
    let frame_h = height as i64;
    for rect in rects {
        let x0 = rect.x.max(0) as usize;
        let y0 = rect.y.max(0) as usize;
        let x1 = (rect.x as i64 + rect.width as i64).clamp(0, frame_w) as usize;
        let y1 = (rect.y as i64 + rect.height as i64).clamp(0, frame_h) as usize;
        if x0 >= x1 || y0 >= y1 {
            continue;
        }
        for y in y0..y1 {
            let row = y * stride;
            for x in x0..x1 {
                let i = row + x * 4;
                if i + 4 <= pixels.len() {
                    pixels[i] = 0;
                    pixels[i + 1] = 0;
                    pixels[i + 2] = 0;
                    pixels[i + 3] = 255;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use observer_model::{normalize_even, ScreenFrame};
    use std::sync::Arc;

    fn win(exe: &str, title: &str, bounds: Option<Rect>) -> WindowInfo {
        WindowInfo {
            exe_name: exe.to_string(),
            title: title.to_string(),
            bounds,
            identity_uncertain: false,
        }
    }

    fn rect(x: i32, y: i32, w: u32, h: u32) -> Rect {
        Rect {
            x,
            y,
            width: w,
            height: h,
        }
    }

    // ── rules ────────────────────────────────────────────────────────────────

    #[test]
    fn default_excludes_private_browsing_only() {
        let r = ExclusionRules::default();
        assert!(r.excluded_exes.is_empty());
        assert!(r.title_patterns.is_empty());
        assert!(r.exclude_private_browsing);
        assert!(r.is_active(), "private-browsing default makes it active");
    }

    #[test]
    fn empty_rules_are_inactive() {
        let r = ExclusionRules {
            excluded_exes: vec![],
            title_patterns: vec![],
            exclude_private_browsing: false,
        };
        assert!(!r.is_active());
    }

    #[test]
    fn normalized_lowercases_trims_dedupes_drops_empty() {
        let r = ExclusionRules {
            excluded_exes: vec![
                " Slack.exe ".into(),
                "SLACK.EXE".into(),
                "".into(),
                "  ".into(),
            ],
            title_patterns: vec!["Reddit".into(), "reddit".into(), "Facebook".into()],
            exclude_private_browsing: true,
        };
        let n = r.normalized();
        assert_eq!(n.excluded_exes, vec!["slack.exe"]);
        assert_eq!(n.title_patterns, vec!["reddit", "facebook"]);
        assert!(n.exclude_private_browsing);
    }

    // ── exe matching (exact, case-insensitive, never substring) ───────────────

    #[test]
    fn exe_match_is_exact_case_insensitive() {
        let rules = ExclusionRules {
            excluded_exes: vec!["slack.exe".into()],
            title_patterns: vec![],
            exclude_private_browsing: false,
        };
        // exact, any case
        assert_eq!(
            evaluate(
                &rules,
                &[win("SLACK.EXE", "Slack", Some(rect(0, 0, 10, 10)))]
            ),
            ExclusionDecision::Redact(vec![rect(0, 0, 10, 10)])
        );
        // NOT a substring victim — the anti-fuzzy invariant
        assert_eq!(
            evaluate(
                &rules,
                &[win("slackbot.exe", "Bot", Some(rect(0, 0, 10, 10)))]
            ),
            ExclusionDecision::Pass
        );
        // unrelated app passes
        assert_eq!(
            evaluate(&rules, &[win("notepad.exe", "x", Some(rect(0, 0, 10, 10)))]),
            ExclusionDecision::Pass
        );
    }

    // ── title-pattern matching (substring, case-insensitive) ──────────────────

    #[test]
    fn title_pattern_substring_case_insensitive() {
        let rules = ExclusionRules {
            excluded_exes: vec![],
            title_patterns: vec!["reddit".into()],
            exclude_private_browsing: false,
        };
        assert_eq!(
            evaluate(
                &rules,
                &[win(
                    "chrome.exe",
                    "REDDIT - dive in",
                    Some(rect(1, 2, 3, 4))
                )]
            ),
            ExclusionDecision::Redact(vec![rect(1, 2, 3, 4)])
        );
        assert_eq!(
            evaluate(
                &rules,
                &[win("chrome.exe", "news site", Some(rect(0, 0, 1, 1)))]
            ),
            ExclusionDecision::Pass
        );
        // empty title never matches
        assert_eq!(
            evaluate(&rules, &[win("chrome.exe", "", Some(rect(0, 0, 1, 1)))]),
            ExclusionDecision::Pass
        );
    }

    // ── private-browsing detection ────────────────────────────────────────────

    #[test]
    fn private_browser_detection_per_family() {
        assert!(is_private_window(
            "chrome.exe",
            "Reddit (Incognito) - Google Chrome"
        ));
        assert!(is_private_window(
            "msedge.exe",
            "Bing - [InPrivate] - Microsoft Edge"
        ));
        assert!(is_private_window(
            "firefox.exe",
            "Mozilla Firefox (Private Browsing)"
        ));
        assert!(is_private_window("brave.exe", "Search (Private) - Brave"));
        // normal windows of the same browsers
        assert!(!is_private_window("chrome.exe", "Reddit - Google Chrome"));
        assert!(!is_private_window("firefox.exe", "Mozilla Firefox"));
        // non-browser with "incognito" in the title is NOT a private-browser match
        assert!(!is_private_window("notepad.exe", "incognito notes.txt"));
    }

    #[test]
    fn private_rule_excludes_only_when_enabled() {
        let on = ExclusionRules {
            excluded_exes: vec![],
            title_patterns: vec![],
            exclude_private_browsing: true,
        };
        let off = ExclusionRules {
            exclude_private_browsing: false,
            ..on.clone()
        };
        let w = win("chrome.exe", "x (Incognito)", Some(rect(0, 0, 5, 5)));
        assert_eq!(
            evaluate(&on, std::slice::from_ref(&w)),
            ExclusionDecision::Redact(vec![rect(0, 0, 5, 5)])
        );
        assert_eq!(
            evaluate(&off, std::slice::from_ref(&w)),
            ExclusionDecision::Pass
        );
    }

    // ── decision assembly + fail-closed ───────────────────────────────────────

    #[test]
    fn no_rules_passes_without_inspecting_windows() {
        let rules = ExclusionRules {
            excluded_exes: vec![],
            title_patterns: vec![],
            exclude_private_browsing: false,
        };
        assert_eq!(
            evaluate(&rules, &[win("slack.exe", "Incognito", None)]),
            ExclusionDecision::Pass
        );
    }

    #[test]
    fn multiple_excluded_windows_accumulate_rects() {
        let rules = ExclusionRules {
            excluded_exes: vec!["slack.exe".into()],
            title_patterns: vec!["secret".into()],
            exclude_private_browsing: false,
        };
        let decision = evaluate(
            &rules,
            &[
                win("slack.exe", "Slack", Some(rect(0, 0, 10, 10))),
                win("notepad.exe", "ok", Some(rect(20, 20, 5, 5))),
                win("chrome.exe", "my secret doc", Some(rect(40, 40, 8, 8))),
            ],
        );
        assert_eq!(
            decision,
            ExclusionDecision::Redact(vec![rect(0, 0, 10, 10), rect(40, 40, 8, 8)])
        );
    }

    #[test]
    fn excluded_window_without_geometry_drops_whole_frame() {
        let rules = ExclusionRules {
            excluded_exes: vec!["slack.exe".into()],
            title_patterns: vec![],
            exclude_private_browsing: false,
        };
        assert_eq!(
            evaluate(&rules, &[win("slack.exe", "Slack", None)]),
            ExclusionDecision::Drop
        );
    }

    #[test]
    fn identity_uncertain_drops_when_any_rule_active() {
        let mut w = win("", "", Some(rect(0, 0, 10, 10)));
        w.identity_uncertain = true;
        // Every active rule type fails closed on an unreadable real window —
        // including exe-only (an excluded app could run elevated under a name we
        // can't query).
        for rules in [
            ExclusionRules {
                excluded_exes: vec!["slack.exe".into()],
                title_patterns: vec![],
                exclude_private_browsing: false,
            },
            ExclusionRules {
                excluded_exes: vec![],
                title_patterns: vec!["secret".into()],
                exclude_private_browsing: false,
            },
            ExclusionRules {
                excluded_exes: vec![],
                title_patterns: vec![],
                exclude_private_browsing: true,
            },
        ] {
            assert_eq!(
                evaluate(&rules, std::slice::from_ref(&w)),
                ExclusionDecision::Drop,
                "uncertain window must drop under {rules:?}"
            );
        }
    }

    #[test]
    fn identity_uncertain_ignored_when_no_rules_active() {
        // With nothing configured, an uncertain window is irrelevant — we never
        // even enumerate in production, and evaluate passes.
        let mut w = win("", "", Some(rect(0, 0, 10, 10)));
        w.identity_uncertain = true;
        let none = ExclusionRules {
            excluded_exes: vec![],
            title_patterns: vec![],
            exclude_private_browsing: false,
        };
        assert_eq!(evaluate(&none, &[w]), ExclusionDecision::Pass);
    }

    // ── redaction ──────────────────────────────────────────────────────────────

    fn solid(w: usize, h: usize, byte: u8) -> Vec<u8> {
        vec![byte; w * h * 4]
    }

    fn px(buf: &[u8], w: usize, x: usize, y: usize) -> [u8; 4] {
        let i = (y * w + x) * 4;
        [buf[i], buf[i + 1], buf[i + 2], buf[i + 3]]
    }

    fn source_px(x: usize, y: usize) -> [u8; 4] {
        [
            x as u8,
            y as u8,
            (x as u8).wrapping_mul(31).wrapping_add(y as u8),
            255,
        ]
    }

    #[test]
    fn redaction_blacks_out_rect_only() {
        let (w, h) = (8usize, 6usize);
        let mut buf = solid(w, h, 200);
        apply_redaction(
            &mut buf,
            w as u32,
            h as u32,
            ScreenPixelFormat::Rgba8,
            &[rect(2, 1, 3, 2)],
        );
        // inside the rect -> opaque black
        for y in 1..3 {
            for x in 2..5 {
                assert_eq!(
                    px(&buf, w, x, y),
                    [0, 0, 0, 255],
                    "({x},{y}) should be black"
                );
            }
        }
        // outside untouched
        assert_eq!(px(&buf, w, 0, 0), [200, 200, 200, 200]);
        assert_eq!(px(&buf, w, 5, 1), [200, 200, 200, 200]);
        assert_eq!(px(&buf, w, 2, 3), [200, 200, 200, 200]);
    }

    #[test]
    fn redaction_clamps_offscreen_and_negative_rects() {
        let (w, h) = (4usize, 4usize);
        let mut buf = solid(w, h, 100);
        // straddles top-left and extends past bottom-right; clamps to the frame
        apply_redaction(
            &mut buf,
            w as u32,
            h as u32,
            ScreenPixelFormat::Bgra8,
            &[rect(-2, -2, 100, 100)],
        );
        for y in 0..h {
            for x in 0..w {
                assert_eq!(px(&buf, w, x, y), [0, 0, 0, 255]);
            }
        }
    }

    #[test]
    fn redaction_ignores_zero_area_and_fully_offscreen() {
        let (w, h) = (4usize, 4usize);
        let mut buf = solid(w, h, 77);
        apply_redaction(
            &mut buf,
            w as u32,
            h as u32,
            ScreenPixelFormat::Rgba8,
            &[rect(0, 0, 0, 0), rect(10, 10, 5, 5), rect(-50, -50, 10, 10)],
        );
        assert!(buf.iter().all(|&b| b == 77), "no pixel should change");
    }

    #[test]
    fn redaction_tolerates_short_buffer_without_panic() {
        let mut buf = vec![50u8; 4 * 4 * 4 - 10]; // 10 bytes short
        apply_redaction(
            &mut buf,
            4,
            4,
            ScreenPixelFormat::Rgba8,
            &[rect(0, 0, 4, 4)],
        );
        // no panic; the in-bounds prefix is blacked, trailing short row left alone
        assert_eq!(&buf[0..4], &[0, 0, 0, 255]);
    }

    #[test]
    fn redaction_then_crop_keeps_redacted_offsets() {
        // AC3 host-test path (D2 path (a)): redact at full capture dimensions, then crop.
        let (w, h) = (5usize, 3usize);
        let mut data = Vec::with_capacity(w * h * 4);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&source_px(x, y));
            }
        }
        let redacted = rect(1, 0, 2, 2);

        apply_redaction(
            &mut data,
            w as u32,
            h as u32,
            ScreenPixelFormat::Rgba8,
            &[redacted],
        );
        let frame = ScreenFrame {
            seq: 0,
            arrival_100ns: 0,
            width: w as u32,
            height: h as u32,
            pixel_format: ScreenPixelFormat::Rgba8,
            pixels: Arc::from(data),
        };
        let cropped = normalize_even(&frame);

        assert_eq!((cropped.width, cropped.height), (4, 2));
        for y in 0..cropped.height as usize {
            for x in 0..cropped.width as usize {
                let expected = if (1..3).contains(&x) && y < 2 {
                    [0, 0, 0, 255]
                } else {
                    source_px(x, y)
                };
                assert_eq!(
                    px(&cropped.pixels, cropped.width as usize, x, y),
                    expected,
                    "pixel ({x},{y})"
                );
            }
        }
    }

    // ── serde ──────────────────────────────────────────────────────────────────

    #[test]
    fn rules_round_trip_json() {
        let r = ExclusionRules {
            excluded_exes: vec!["slack.exe".into()],
            title_patterns: vec!["reddit".into()],
            exclude_private_browsing: false,
        };
        let json = serde_json::to_string(&r).unwrap();
        let back: ExclusionRules = serde_json::from_str(&json).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn missing_private_key_defaults_true() {
        // A persisted file written before the field existed (or hand-edited)
        // deserializes private-browsing to the trust-forward default.
        let back: ExclusionRules = serde_json::from_str(r#"{"excluded_exes":[]}"#).unwrap();
        assert!(back.exclude_private_browsing);
        assert!(back.title_patterns.is_empty());
    }
}
