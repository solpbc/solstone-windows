// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

// FlaUI/UIA3 smoke driver for the installed observer.
//
// The acceptance oracle is the health dump, NOT the webview DOM:
//   Tier 0  poll --dump-state / the health endpoint against the contract's
//           token vocabulary until app_state == "observing" (deterministic).
//   Tier 1  drive the native tray icon + menu by AutomationId (Tauri exposes
//           these reliably on the Win32/UIA surface).
//   Tier 2  webview data-automation-id is best-effort; the green path must NOT
//           depend on Chromium UIA resolving.
//
// Assert on a UIA *value* pattern on a stable element — never an accessible-name
// on a relabeled control (a spike gotcha). AutomationIds come from the committed
// automation-contract.json (the single source of truth).

namespace Solstone.Harness
{
    internal static class Program
    {
        private static int Main(string[] args)
        {
            // Skeleton. The ui-driver spike graduates here into the real net48
            // driver: launch/attach to the installed app, find the tray by
            // AutomationId, poll health to `observing`, support a
            // failure-injection mode (kill system audio) that asserts the drop
            // out of `observing`.
            System.Console.WriteLine("solstone FlaUI driver: not yet implemented (graduates from the ui-driver spike).");
            return 0;
        }
    }
}
