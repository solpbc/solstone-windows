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
- The tray-resident honest-state shell is wired: per-state tray icon and menu,
  on-demand Settings/About windows, and a pure-renderer webview fed by the live
  health stream.
- The fixed loopback `/healthz` + `--dump-state` oracle now reports the running
  app's live health snapshot, falling back to an honest not-running snapshot when
  the app is absent.
- Session/power/display notifications now flow into the engine command channel
  from the Windows notification pump thread.
- `capture-engine` has an `EngineCommand` channel and change-driven health watch
  for shell and lifecycle integration.
- The observer binary is now Velopack-aware: `VelopackApp` runs first in `main()`,
  handling the install/updated/obsolete/uninstall lifecycle hooks (so they exit 0);
  the first launch after install registers the per-user autostart login item.
- Real `make package`: `vpk pack` of the release binary into `Releases/` (per-user
  `%LocalAppData%`, no UAC, unsigned) — full + delta `nupkg`, `Setup.exe`, and the
  `releases.win.json` update feed. The packed version equals the binary's
  `--dump-state` version by construction.
- Real `make publish`: uploads `Releases/` to GitHub Releases (the monotonic update
  feed), feed JSON last, fail-loud on an existing tag.
- FlaUI smoke harness: the net48 `harness/driver/` is implemented (graduated from
  the reference spike) and `make smoke` runs it against the *installed* app. The
  deterministic gate is the health oracle (Tier 0: poll loopback `/healthz` until
  `app_state` reaches the contract's `observing` token); Tier 1 drives the native
  tray chrome by AutomationId; the webview DOM is never load-bearing. A
  `--fail-inject` mode asserts the observer honestly leaves `observing` when a
  required source is killed, and a `--selftest` mode proves the contract-parse /
  token-match / drop-detection logic with no live target. AutomationIds and the
  `observing` token are read from `automation-contract.json`, never hardcoded.
- **Pairing + upload (Wave 2): the observer pairs to a journal and delivers its
  segments.** Two new crates implement a faithful Rust client of the same observer
  wire protocol the iOS and Android apps ship and the journal serves — no new
  crypto, no new wire format:
  - `observer-pl` (pure, host-testable): the `go.solstone.app/p#…` pair-link parser
    (Crockford base32, v04 single- + v05 multi-address), the spl mux framing
    (8-byte header, OPEN/DATA/CLOSE/PING/PONG), HTTP-over-PL request/response, the
    observer register/ingest/heartbeat/reconcile wire types, the multipart body
    (the `files` field the journal reads), CA-fingerprint prefix pinning, and the
    epoch→`day`/`segment` key conversion. Round-trip unit-tested end to end.
  - `pl-transport-win` (transport): the framed-mTLS connection over rustls (ring)
    with CA-fingerprint pinning **and** handshake-signature verification, EC P-256
    key + CSR generation, the pairing handshake, the observer client
    (register/ingest/heartbeat/reconcile), the sealed-segment store, and the upload
    coordinator + heartbeat loop. Connection-level retry tolerates a freshly-paired
    fingerprint not yet seen by every journal worker.
- The upload coordinator ships sealed segments to `/app/observer/ingest` and
  **reconciles by sha256** before deleting the local copy — a segment counts as
  delivered only after the journal confirms it landed (honest state, earned not
  asserted), with exponential backoff on failure.
- The heartbeat posts `observe.status` to the paired journal every 15s, carrying the
  real pause state from the health dump.
- The Settings window gains a **Pairing pane** (paste a pair-link, see the pairing
  phase + paired journal) and a **journal-sync** line in Status; pairing/upload
  state is surfaced in the `HealthDump` (`sync` field) so `--dump-state` / `/healthz`
  reflect it. The AutomationId contract gains the pairing/upload ids and the
  `pairing_phase` token vocabulary.

### Changed

- `tauri.conf.json` bundle icon points at `icons/icon.ico` (was a placeholder PNG),
  so the installer + executable carry the brand mark.
- WGC raw screen capture is capped at approximately 1 fps. At 1080p RGBA8 this is
  roughly 2.5 GB per five-minute segment and 15 GB per 30-minute soak; an encoder
  remains deferred.
- The tray is now built in code with the contract id `tray.root`; the config tray
  block is removed to avoid a duplicate tray resource.

### Fixed

- Segment writes are now durably synced per chunk. Previously a segment's frames
  stayed in the OS write cache until finalize, so a crash mid-segment lost the
  whole in-flight segment and incomplete-segment recovery read a zero length for a
  segment that had actually captured data (wrongly quarantining it). Found in
  on-device validation: live capture reaches `observing` and writes real frames,
  but the on-disk segment showed 0 bytes until rotation. At the capped ~1 fps the
  per-chunk sync is negligible.

### Documentation

- Mic capture: validating against a *real* input device is deferred to a future
  iteration. The `NoInputDevice` state stays modeled and tested from the first
  wave; live-mic capture is not a release gate (`docs/lifecycle-matrix.md`).
- The FlaUI-against-the-real-shell reconfirm is folded into the first wave (it needs
  the shell to exist) rather than a pre-wave step (`harness/README.md`).
