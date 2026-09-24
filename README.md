# solstone-windows

The solstone app for windows, part of [solstone](https://solstone.app): a
per-user, non-elevated tray app. It takes in what you share with it (your
screen, system audio, and your microphone when one is present), and all of it
goes into your journal.

## Status

Shipped alpha. Screen, system audio and microphone intake, the tray shell,
pairing with a journal, signed Velopack packaging with delta updates, and
the FlaUI smoke gate are all in place and releasing (see
[CHANGELOG.md](CHANGELOG.md)). It pairs with a journal; it does not run one.

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

Everything the app takes in is written locally first, then goes into your
journal. There is no analytics, telemetry, tracking, or crash reporting. The app
reaches your journal directly or through a relay (by default the one sol pbc
runs at link.solstone.app). The only other request it makes on its own is an
update check to updates.solstone.app, which carries no app version or install
id. If you turn on automatic downloads, the update itself comes from the same
place. State is always earned: the app never shows "on" unless it truly is.

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
