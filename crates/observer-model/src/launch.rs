// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

pub const FROM_AUTOSTART_ARG: &str = "--from-autostart";

const SUPPRESS: &[&str] = &[
    FROM_AUTOSTART_ARG,
    "--dump-state",
    "--healthz",
    "--check-update",
    "--apply-update",
    "--dump-windows",
    "--log-path",
    "--integration",
    "--open-journal",
    "--veloapp-install",
    "--veloapp-updated",
    "--veloapp-obsolete",
    "--veloapp-uninstall",
];

pub fn launch_should_surface<S: AsRef<str>>(args: &[S]) -> bool {
    !args.iter().any(|arg| SUPPRESS.contains(&arg.as_ref()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bare_launch_surfaces() {
        assert!(launch_should_surface::<&str>(&[]));
    }

    #[test]
    fn explicit_open_view_surfaces() {
        assert!(launch_should_surface(&["--open-view", "settings"]));
    }

    #[test]
    fn autostart_suppresses() {
        assert!(!launch_should_surface(&[FROM_AUTOSTART_ARG]));
    }

    #[test]
    fn cli_verbs_suppress() {
        assert!(!launch_should_surface(&["--dump-state"]));
        assert!(!launch_should_surface(&["--healthz"]));
        assert!(!launch_should_surface(&["--check-update"]));
        assert!(!launch_should_surface(&["--apply-update"]));
        assert!(!launch_should_surface(&["--dump-windows"]));
        assert!(!launch_should_surface(&["--log-path"]));
        assert!(!launch_should_surface(&["--open-journal"]));
    }

    #[test]
    fn integration_mode_suppresses() {
        // The operator mode is headless: it must never surface a window, with or
        // without its operation argument.
        assert!(!launch_should_surface(&["--integration"]));
        assert!(!launch_should_surface(&["--integration", "pair"]));
        assert!(!launch_should_surface(&[
            "--integration",
            "roundtrip",
            "--deadline-secs",
            "30"
        ]));
    }

    #[test]
    fn a_bare_launch_is_untouched_by_the_integration_entry() {
        // The negative half of the same guarantee: normal GUI startup only
        // changes when the flag is actually present.
        assert!(launch_should_surface::<&str>(&[]));
        assert!(launch_should_surface(&["--open-view", "home"]));
        assert!(launch_should_surface(&["integration"]));
        assert!(launch_should_surface(&["--integration-like"]));
    }

    #[test]
    fn veloapp_verbs_suppress() {
        assert!(!launch_should_surface(&["--veloapp-install"]));
        assert!(!launch_should_surface(&["--veloapp-updated"]));
        assert!(!launch_should_surface(&["--veloapp-obsolete"]));
        assert!(!launch_should_surface(&["--veloapp-uninstall"]));
    }

    #[test]
    fn autostart_suppresses_even_with_open_view() {
        assert!(!launch_should_surface(&[
            FROM_AUTOSTART_ARG,
            "--open-view",
            "settings"
        ]));
    }
}
