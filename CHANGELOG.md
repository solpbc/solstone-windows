# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Initial public bootstrap: the cargo workspace, the three crate tiers
  (pure / platform / composition), the Tauri v2 tray-app skeleton, the Vite
  webview skeleton, the net48 FlaUI harness skeleton, Velopack packaging scaffold,
  the `make` verb surface, and the generated, drift-gated `automation-contract.json`.
- The pure tier is host-testable: rotation math, the honest-state reducer,
  incomplete-segment recovery, the backoff/circuit-breaker, and the contract
  generator all run off-Windows.
- `src-tauri/icons/icon.ico` — a real multi-resolution app icon (16/24/32/48/64/
  128/256, rendered natively at each size from the solstone brand mark). This
  unblocks the Tauri app-crate build and the heavier `make build` / `make package`
  path, which embed the `.ico` as the executable's Windows resource. Verified: the
  app crate now compiles and links on the Windows build box.
- The plumbing **reference spikes** are imported under `spikes/` (excluded from the
  workspace build): `gdi-screen`, `wasapi-loopback`, `wgc`, `mic`, `flaui-scratch`,
  and `flaui-driver`. Each is the source as spiked, trimmed to the source itself.
- Capture-core platform landing: real WGC screen and WASAPI system-audio/mic source
  implementations, std-backed segment writing and incomplete-segment recovery with a
  staleness guard, the per-session single-instance mutex, and the session/power/display
  notification pump.
- The injected capture sink, clock, and segment-writer seams, plus the computed
  `HealthDump` and loopback `/healthz` server, are wired for host-testable engine
  operation.

### Changed

- `tauri.conf.json` bundle icon points at `icons/icon.ico` (was a placeholder PNG),
  so the installer + executable carry the brand mark.
- WGC raw screen capture is capped at approximately 1 fps. At 1080p RGBA8 this is
  roughly 2.5 GB per five-minute segment and 15 GB per 30-minute soak; an encoder
  remains deferred.

### Documentation

- Mic capture: validating against a *real* input device is deferred to a future
  iteration. The `NoInputDevice` state stays modeled and tested from the first
  wave; live-mic capture is not a release gate (`docs/lifecycle-matrix.md`).
- The FlaUI-against-the-real-shell reconfirm is folded into the first wave (it needs
  the shell to exist) rather than a pre-wave step (`harness/README.md`).
