# harness — FlaUI / UIA smoke driver

A .NET FlaUI driver that smoke-tests the **installed** observer. It is **not** a
cargo workspace member; it builds with the .NET SDK and runs on the Windows build
box against the live target (`make smoke`).

## net48 + Accessibility.dll (the load-bearing gotcha)

The driver targets **net48 on purpose**. FlaUI's UIA3 backend needs
`Accessibility.dll` (the `IAccessible` COM interop) present in the **publish
layout** to bind at runtime. A `net8.0-windows` build will not reliably ship it,
so UIA3 fails to attach. net48 provides it; `Driver.csproj` also references
`Accessibility` explicitly to force it into the publish output. Do not "upgrade"
the target framework without re-proving UIA3 binding in the published layout.

## What the smoke asserts (and what it must not depend on)

- **Tier 0 — the health dump is the oracle.** The "reached observing" assertion
  is polling `--dump-state` / the health endpoint against the contract's token
  vocabulary. Robust against any webview UIA quirk.
- **Tier 1 — native chrome is authoritative for interaction.** Find the window /
  tray by AutomationId (from the committed `automation-contract.json`) and invoke
  tray/menu items on the native Win32/UIA surface. This tier is advisory in
  `make smoke`; the release gate is Tier 0 + Tier R.
- **Tier R — view render beacon is load-bearing.** The Settings view must report
  `views.settings == rendered` on `/healthz`, proving our webview UI loaded and
  painted.
- **Tier 2 — webview `data-automation-id` is best-effort.** Stamped from the same
  source of truth, but the green path must **not** depend on Chromium UIA in
  WebView2 resolving.

Assert against a UIA **value** pattern on a stable element — never an
accessible-name on a relabeled control (the other spike gotcha).

## Reconfirm against the real shell (first-wave work)

The plumbing spike proved FlaUI/UIA3 green against a **native WinForms** target
(`spikes/flaui-scratch/`) — not against the real Tauri shell. A Tauri window hosts
a Chromium **WebView2**, whose UIA tree is structurally different. Reconfirming
that the harness reaches the chrome we control (tray + window frame + stamped
AutomationIds), and settling on the Tier 0 health-dump + Tier 1 native-chrome
assertions above, **needs the shell to exist first** — so it is part of the
first-wave shell + smoke work (1B builds the shell; 1D builds and reconfirms the
smoke), not a pre-wave step. Until then, Tier 2 webview DOM stays explicitly
best-effort.

## Run

`scripts/smoke.ps1` registers + fires a low-privilege scheduled task
(`LogonType=Interactive`) into Session 1 only to launch the installed observer.
The load-bearing driver then runs directly in the SSH/Session-0 context and
polls loopback `/healthz` for `observing` plus the Settings render beacon. The
FlaUI/UIA native-chrome pass runs afterward in Session 1 as a bounded advisory
step and cannot decide `SMOKE_OK` / `SMOKE_FAIL`. A failure-injection mode (kill
system audio) asserts the drop out of `observing` and exits non-zero.

The `ui-driver` reference spike graduates into `driver/` as the real harness.
