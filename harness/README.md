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
  tray/menu items on the native Win32/UIA surface.
- **Tier 2 — webview `data-automation-id` is best-effort.** Stamped from the same
  source of truth, but the green path must **not** depend on Chromium UIA in
  WebView2 resolving.

Assert against a UIA **value** pattern on a stable element — never an
accessible-name on a relabeled control (the other spike gotcha).

## Run

`scripts/smoke.ps1` registers + fires a low-privilege scheduled task
(`LogonType=Interactive`) into Session 1, runs the published net48 driver, polls
health to `observing`, and exits 0; a failure-injection mode (kill system audio)
asserts the drop out of `observing` and exits non-zero.

The `ui-driver` reference spike graduates into `driver/` as the real harness.
