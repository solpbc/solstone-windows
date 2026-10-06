// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Host-mode recognition from argv.
//!
//! `main` calls [`classify`] before Velopack's hooks or any other init. A
//! native-messaging invocation never reaches the tray app: it either becomes a
//! host (allowlisted extension id) or is refused and exits without a window.
//!
//! The browsers' Windows argv shapes:
//! - Chrome and Edge: `chrome-extension://<id>/` then `--parent-window=<hwnd>`
//!   (the second argument may be absent on older builds);
//! - Firefox: `<manifest path> <extension id>`.
//!
//! The id comparison is a consistency check, not caller authentication: any
//! same-user process can start the host with a forged argument.

use native_browser_frame::constants::{
    DEV_CHROME_ID, DEV_EDGE_ID, DEV_FIREFOX_ID, PROD_CHROME_ID, PROD_EDGE_ID, PROD_FIREFOX_ID,
};

/// Which registration set an invocation matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Production,
    Development,
}

impl Mode {
    pub fn token(self) -> &'static str {
        match self {
            Mode::Production => "production",
            Mode::Development => "development",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "production" => Some(Mode::Production),
            "development" => Some(Mode::Development),
            _ => None,
        }
    }
}

/// The browser family the launch shape implies. The extension's own `hello`
/// names the exact brand; the app checks the two agree.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrandHint {
    Chromium,
    Firefox,
}

impl BrandHint {
    pub fn token(self) -> &'static str {
        match self {
            BrandHint::Chromium => "chromium",
            BrandHint::Firefox => "firefox",
        }
    }

    pub fn parse(token: &str) -> Option<Self> {
        match token {
            "chromium" => Some(BrandHint::Chromium),
            "firefox" => Some(BrandHint::Firefox),
            _ => None,
        }
    }

    /// Whether the extension's declared brand fits this launch shape.
    pub fn admits(self, brand: &str) -> bool {
        match self {
            BrandHint::Chromium => brand == "chrome" || brand == "edge",
            BrandHint::Firefox => brand == "firefox",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Invocation {
    pub brand: BrandHint,
    pub mode: Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Classified {
    /// Not a native-messaging launch: continue with the normal app.
    NotHost,
    /// An allowlisted extension: run the host relay.
    Host(Invocation),
    /// Native-messaging shaped but not allowlisted (or host mode is compiled
    /// out): exit at once, open nothing.
    Rejected,
}

const CHROME_ORIGIN_PREFIX: &str = "chrome-extension://";
const PARENT_WINDOW_PREFIX: &str = "--parent-window=";

/// Whether argv has a native-messaging shape at all (the one definition lives
/// in `observer-model`, so `main` can refuse such a launch even in a build
/// without host mode).
pub fn looks_like_native_messaging(args: &[String]) -> bool {
    observer_model::is_native_messaging_launch(args)
}

/// Classify argv (program name already stripped). `development` admits the
/// development ids in addition to production; no release build sets it.
pub fn classify(args: &[String], development: bool) -> Classified {
    if !looks_like_native_messaging(args) {
        return Classified::NotHost;
    }
    if let Some(origin) = args.first().filter(|a| a.starts_with(CHROME_ORIGIN_PREFIX)) {
        // Chrome/Edge: the origin, optionally `--parent-window=<digits>`.
        match args.get(1) {
            None => {}
            Some(arg) if is_parent_window(arg) && args.len() == 2 => {}
            Some(_) => return Classified::Rejected,
        }
        let Some(id) = origin
            .strip_prefix(CHROME_ORIGIN_PREFIX)
            .and_then(|rest| rest.strip_suffix('/'))
        else {
            return Classified::Rejected;
        };
        let mode = if id == PROD_CHROME_ID || id == PROD_EDGE_ID {
            Mode::Production
        } else if development && (id == DEV_CHROME_ID || id == DEV_EDGE_ID) {
            Mode::Development
        } else {
            return Classified::Rejected;
        };
        return Classified::Host(Invocation {
            brand: BrandHint::Chromium,
            mode,
        });
    }
    if args.len() != 2 || args[0].starts_with(CHROME_ORIGIN_PREFIX) {
        return Classified::Rejected;
    }
    let id = args[1].as_str();
    let mode = if id == PROD_FIREFOX_ID {
        Mode::Production
    } else if development && id == DEV_FIREFOX_ID {
        Mode::Development
    } else {
        return Classified::Rejected;
    };
    Classified::Host(Invocation {
        brand: BrandHint::Firefox,
        mode,
    })
}

fn is_parent_window(arg: &str) -> bool {
    arg.strip_prefix(PARENT_WINDOW_PREFIX)
        .is_some_and(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(args: &[&str]) -> Vec<String> {
        args.iter().map(|s| s.to_string()).collect()
    }

    const PROD_ORIGIN: &str = "chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim/";
    const DEV_ORIGIN: &str = "chrome-extension://fgfnkcefedeheoeamppkiiloncfekakf/";

    #[test]
    fn normal_launches_are_not_host() {
        for args in [
            v(&[]),
            v(&["--dump-state"]),
            v(&["--veloapp-updated", "2.0.16"]),
            v(&["--from-autostart"]),
            v(&["--open-view", "status"]),
        ] {
            assert_eq!(classify(&args, true), Classified::NotHost, "{args:?}");
        }
    }

    #[test]
    fn chromium_production_with_and_without_parent_window() {
        let host = Classified::Host(Invocation {
            brand: BrandHint::Chromium,
            mode: Mode::Production,
        });
        assert_eq!(
            classify(&v(&[PROD_ORIGIN, "--parent-window=0"]), false),
            host
        );
        assert_eq!(
            classify(&v(&[PROD_ORIGIN, "--parent-window=132458"]), false),
            host
        );
        assert_eq!(classify(&v(&[PROD_ORIGIN]), false), host);
    }

    #[test]
    fn firefox_production_with_a_spaced_non_ascii_manifest_path() {
        let args = v(&[
            r"C:\Users\Zoë Q\AppData\Local\SolstoneOwner\browser\app.solstone.browser.firefox.json",
            "browser@solstone.app",
        ]);
        assert_eq!(
            classify(&args, false),
            Classified::Host(Invocation {
                brand: BrandHint::Firefox,
                mode: Mode::Production
            })
        );
    }

    #[test]
    fn dev_ids_are_refused_by_argv_unless_development_is_compiled_in() {
        assert_eq!(
            classify(&v(&[DEV_ORIGIN, "--parent-window=0"]), false),
            Classified::Rejected
        );
        assert_eq!(
            classify(&v(&["m.json", "browser.dev@solstone.app"]), false),
            Classified::Rejected
        );
        assert_eq!(
            classify(&v(&[DEV_ORIGIN, "--parent-window=0"]), true),
            Classified::Host(Invocation {
                brand: BrandHint::Chromium,
                mode: Mode::Development
            })
        );
        assert_eq!(
            classify(&v(&["m.json", "browser.dev@solstone.app"]), true),
            Classified::Host(Invocation {
                brand: BrandHint::Firefox,
                mode: Mode::Development
            })
        );
    }

    #[test]
    fn malformed_native_messaging_shapes_are_refused_not_launched() {
        for args in [
            v(&["chrome-extension://unknownunknownunknownunknownabcd/"]),
            v(&["chrome-extension://eibbeeoifjoabddfmgeggnageolkcnim"]),
            v(&[PROD_ORIGIN, "--veloapp-updated"]),
            v(&[PROD_ORIGIN, "--parent-window="]),
            v(&[PROD_ORIGIN, "--parent-window=0", "extra"]),
            v(&["--veloapp-updated", PROD_ORIGIN]),
            v(&["m.json", "someone-else@example.com"]),
            v(&[PROD_ORIGIN, "browser@solstone.app"]),
        ] {
            assert_eq!(classify(&args, true), Classified::Rejected, "{args:?}");
        }
    }

    #[test]
    fn brand_hint_admits_only_its_family() {
        assert!(BrandHint::Chromium.admits("chrome"));
        assert!(BrandHint::Chromium.admits("edge"));
        assert!(!BrandHint::Chromium.admits("firefox"));
        assert!(BrandHint::Firefox.admits("firefox"));
        assert!(!BrandHint::Firefox.admits("chrome"));
    }
}
