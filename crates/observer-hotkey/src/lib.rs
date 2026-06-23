// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Global pause/resume hotkey model — pure tier.
//!
//! Owns the owner-configured combo ([`HotkeyConfig`], persisted to `hotkey.json`
//! and edited over IPC), its human formatting, validation, and the honest
//! registration outcome ([`HotkeyRegistration`]) the Settings UI renders.
//!
//! The platform tier (`platform-win`) translates a config into a Win32
//! `RegisterHotKey` call and reports back which [`HotkeyRegistration`] resulted —
//! including the single-registrant truth: a combo another application already
//! owns is reported as [`HotkeyRegistration::ComboTaken`], a visible error, never
//! a silent no-op. This crate has no platform dependency and is host-testable.

#![forbid(unsafe_code)]

use serde::{Deserialize, Serialize};

/// Owner-configured global hotkey. `vk` is the Win32 virtual-key code of the main
/// key (e.g. `0x50` = `P`); the booleans are its modifiers. A usable global hotkey
/// requires at least one modifier and a key — see [`HotkeyConfig::is_armed`].
///
/// The default is all-unset / disabled — the owner opts in (macOS ships no global
/// hotkey at all). Every field is `#[serde(default)]` so a `hotkey.json` written
/// before a field existed (or hand-edited) still deserializes cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct HotkeyConfig {
    /// Whether the owner has the global hotkey turned on.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub ctrl: bool,
    #[serde(default)]
    pub alt: bool,
    #[serde(default)]
    pub shift: bool,
    #[serde(default)]
    pub win: bool,
    /// Win32 virtual-key code of the main key; `0` means unset.
    #[serde(default)]
    pub vk: u32,
}

impl HotkeyConfig {
    /// True when this is a registerable combo: enabled, a key set, and at least
    /// one modifier. A bare-key global hotkey would hijack normal typing, so a
    /// modifier is required — the same rule a sane shortcut picker enforces.
    pub fn is_armed(&self) -> bool {
        self.enabled && self.has_combo()
    }

    /// Whether a key + at least one modifier are present, regardless of `enabled`
    /// (distinguishes "set but turned off" from "never set").
    pub fn has_combo(&self) -> bool {
        self.vk != 0 && (self.ctrl || self.alt || self.shift || self.win)
    }

    /// Human label, e.g. `"Ctrl+Shift+P"`. Empty when no key is set.
    pub fn format(&self) -> String {
        if self.vk == 0 {
            return String::new();
        }
        let mut parts: Vec<&str> = Vec::new();
        if self.ctrl {
            parts.push("Ctrl");
        }
        if self.alt {
            parts.push("Alt");
        }
        if self.shift {
            parts.push("Shift");
        }
        if self.win {
            parts.push("Win");
        }
        let key = vk_label(self.vk);
        if parts.is_empty() {
            key
        } else {
            format!("{}+{}", parts.join("+"), key)
        }
    }
}

/// The honest outcome of trying to register the configured hotkey with the OS.
/// Reported by the platform tier; rendered by Settings so the owner always knows
/// whether their combo is actually live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum HotkeyRegistration {
    /// No hotkey configured or it is turned off — nothing is registered.
    #[default]
    Inactive,
    /// The combo is registered with the OS and live.
    Registered,
    /// Another application already owns this combo — it could **not** be
    /// registered. Surfaced as a visible error, never a silent no-op; the owner
    /// must choose a different combo.
    ComboTaken,
    /// Registration failed for some other reason. The platform tier logs the
    /// specifics; the pure model carries only the coarse outcome.
    Failed,
}

/// What Settings renders: the current config plus its live registration outcome.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct HotkeyView {
    pub config: HotkeyConfig,
    pub registration: HotkeyRegistration,
}

/// A human label for a Win32 virtual-key code. Covers the keys a shortcut is
/// likely to use; falls back to a hex code so an unmapped key still shows
/// something honest rather than nothing.
pub fn vk_label(vk: u32) -> String {
    match vk {
        0x30..=0x39 => ((b'0' + (vk - 0x30) as u8) as char).to_string(), // 0-9
        0x41..=0x5A => ((b'A' + (vk - 0x41) as u8) as char).to_string(), // A-Z
        0x70..=0x7B => format!("F{}", vk - 0x70 + 1),                    // F1-F12
        0x20 => "Space".to_string(),
        _ => format!("0x{vk:02X}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_unset_and_inert() {
        let c = HotkeyConfig::default();
        assert!(!c.is_armed());
        assert!(!c.has_combo());
        assert_eq!(c.format(), "");
        assert_eq!(HotkeyRegistration::default(), HotkeyRegistration::Inactive);
    }

    #[test]
    fn armed_requires_enabled_key_and_modifier() {
        // key + modifier but disabled -> has_combo but not armed
        let off = HotkeyConfig {
            enabled: false,
            ctrl: true,
            shift: true,
            vk: 0x50,
            ..Default::default()
        };
        assert!(off.has_combo());
        assert!(!off.is_armed());

        // enabled but no modifier -> not a registerable combo
        let bare = HotkeyConfig {
            enabled: true,
            vk: 0x50,
            ..Default::default()
        };
        assert!(!bare.has_combo());
        assert!(!bare.is_armed());

        // enabled + modifier + key -> armed
        let armed = HotkeyConfig {
            enabled: true,
            ctrl: true,
            shift: true,
            vk: 0x50,
            ..Default::default()
        };
        assert!(armed.is_armed());
    }

    #[test]
    fn format_renders_modifiers_then_key() {
        let c = HotkeyConfig {
            enabled: true,
            ctrl: true,
            shift: true,
            vk: 0x50,
            ..Default::default()
        };
        assert_eq!(c.format(), "Ctrl+Shift+P");

        let f = HotkeyConfig {
            enabled: true,
            alt: true,
            win: true,
            vk: 0x71,
            ..Default::default()
        };
        assert_eq!(f.format(), "Alt+Win+F2");
    }

    #[test]
    fn vk_label_covers_common_keys_and_falls_back() {
        assert_eq!(vk_label(0x41), "A");
        assert_eq!(vk_label(0x5A), "Z");
        assert_eq!(vk_label(0x30), "0");
        assert_eq!(vk_label(0x39), "9");
        assert_eq!(vk_label(0x70), "F1");
        assert_eq!(vk_label(0x7B), "F12");
        assert_eq!(vk_label(0x20), "Space");
        assert_eq!(vk_label(0x1B), "0x1B"); // Esc -> honest hex fallback
    }

    #[test]
    fn config_round_trips_and_missing_fields_default() {
        let c = HotkeyConfig {
            enabled: true,
            ctrl: true,
            alt: false,
            shift: true,
            win: false,
            vk: 0x50,
        };
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(serde_json::from_str::<HotkeyConfig>(&json).unwrap(), c);

        // A file written before a field existed (or hand-edited) defaults cleanly.
        let partial: HotkeyConfig = serde_json::from_str(r#"{"enabled":true,"vk":80}"#).unwrap();
        assert!(partial.enabled);
        assert_eq!(partial.vk, 80);
        assert!(!partial.ctrl);
    }
}
