# solstone-windows

A Windows-native observer for [solstone](https://solstone.app): a per-user,
non-elevated, tray-resident app that gathers screen and system audio — plus the
microphone when one is present — into local, owner-controlled segments for the
owner's journal.

## Status

Shipped alpha. Live screen + system-audio + microphone capture, the tray shell,
pairing and upload to a journal, signed Velopack packaging with delta
auto-updates, and the FlaUI smoke gate are all in place and releasing (see
[CHANGELOG.md](CHANGELOG.md)). It is a pairing client, not a journal host.

## Layout

```text
crates/
  observer-model/      shared vocabulary + source traits + the HealthDump payload
  observer-segment/    5-minute clock-boundary rotation math
  observer-state/      honest state reducer (Observing is computed, never set)
  observer-health/     --dump-state / /healthz serialization
  observer-recovery/   incomplete-segment scan and finalize
  observer-lifecycle/  backoff + circuit-breaker restart policy
  observer-contract/   AutomationId source of truth + contract generator
  capture-wgc/         Windows.Graphics.Capture screen source
  capture-wasapi/      WASAPI system audio + microphone
  platform-win/        session/power, single-instance, %LocalAppData%, fs
  capture-engine/      the orchestrator (Tauri-agnostic, host-testable)
src-tauri/             the tray-resident binary
ui/                    the WebView2 front-end (vanilla TS + Vite)
xtask/                 the workspace task runner
harness/               the net48 FlaUI smoke driver
packaging/             Velopack config + hooks + signing seam
docs/                  architecture, contract, runbook, lifecycle
spikes/                reference-only code (excluded from the build)
```

## Privacy

The observer writes local, owner-controlled data for the owner's journal. There
is no analytics, telemetry, tracking, or crash reporting, and nothing phones
home. State is always earned: the app never shows "observing" unless it truly is.

## Build & test

```bash
make test    # the pure tier runs on any host (no Windows toolchain needed)
make ci      # fmt · clippy · contract drift · tests · cargo-deny
make build   # the binary + the webview bundle (Windows build box)
```

See [INSTALL.md](INSTALL.md) for prerequisites and [AGENTS.md](AGENTS.md) for the
full development guide.

## License

AGPL-3.0-only. See [LICENSE](LICENSE).
