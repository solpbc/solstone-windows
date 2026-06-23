# Changelog

All notable changes to `solstone-windows` are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); this project adheres to
[Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Velopack release packaging now threads per-release notes into the update feed:
  `scripts/package.ps1` extracts this CHANGELOG's `## [<version>]` section and
  passes it to `vpk pack --releaseNotes`, so `releases.win.json` carries
  `NotesMarkdown`/`NotesHtml`. The in-app Updates pane and
  `solstone.app/releases/windows` render those notes (the Windows analog of the
  macOS appcast `<description>`). A signed release pack requires the section and
  fails loud without it; unsigned dev/local packs pack note-less. Before a signed
  release, cut `## [Unreleased]` to `## [<version>] - <date>` (see the release
  runbook).
- Release-artifact code signing is wired into the Velopack packaging path
  (`scripts/package.ps1`): release artifacts are signed with the sol pbc
  certificate (DigiCert KeyLocker via Velopack's `--signTemplate`), gated
  **opt-in and release-only** (`-Sign` / `SOLSTONE_SIGN=1`) so dev/local and
  delta-validation packs stay unsigned. Adds `packaging/signing/preflight-auth.ps1`,
  a fail-fast credential pre-check. Signing credentials and the keypair alias are
  env-supplied, never committed. A signed `Setup.exe` clears the Windows
  SmartScreen unknown-publisher block on a clean machine.
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
  handling the install/updated/obsolete/uninstall lifecycle hooks (so they exit 0).
- Relaunch-at-login: the observer registers a per-user autostart login item — a
  single named value under the `HKCU\…\CurrentVersion\Run` key, no admin and no
  machine-wide entry — so the tray-resident observer comes back in interactive
  Session 1 after a reboot. Registration is ensured idempotently on every launch
  (it writes only when the entry is missing or stale, so it self-heals an
  unregistered install, re-points the entry if the executable moves, and never
  leaves a duplicate); the Velopack uninstall hook removes it. Replaces the prior
  one-shot first-run registration, which silently left the observer unregistered
  whenever the first post-install launch wasn't the installer-spawned one.
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
- **Mux WINDOW flow-control: segments larger than the 1 MiB initial window now
  upload correctly.** The upload now paces the request body to the journal's
  advertised send window and resumes on the `WINDOW` grants the journal emits as it
  consumes the body (it replenishes at 50% consumed) — the same credit loop the iOS
  client ships, byte-identical to the journal's `framing.py`/`mux.py`. Before this,
  the body was sent in one burst and was only correct for payloads under 1 MiB; an
  encoded screen segment (tens of MB) far exceeds that. `observer-pl` gains the pure,
  host-tested `WindowedUpload` credit state machine and `WINDOW`-frame parsing; the
  transport drives it full-duplex (write up to credit → read grants → repeat),
  proven by a >1 MiB round-trip over real TLS + framing against a window-enforcing
  peer.
- **Screen H.264 encoder (Media Foundation): the screen is encoded to a compact
  `.mp4` per segment instead of stored as raw frames.** A new pure `observer-nv12`
  crate (RGBA/BGRA → NV12, host-tested against known colour vectors) and a platform
  `capture-screen-encode` crate (a Media Foundation `IMFSinkWriter` worker, with
  `windows-rs`/COM `unsafe` quarantined to the platform tier) encode the WGC screen
  frames to H.264 — ~1 Mbps / 1 fps / ~90-frame GOP / native resolution, matching
  the macOS recorder; hardware encoder MFT where present, software-MFT fallback
  otherwise. The engine owns a pure `ScreenEncoder` seam: it opens the encoder
  lazily on the first frame (a frameless window leaves no orphan mp4), writes
  `display_<n>_screen.mp4` (the journal's screen-video filename), and seals a
  segment **only after `Finalize()` succeeds** — a finalize failure leaves the
  segment `.incomplete` (recovery prefers the still-usable audio over a moov-less,
  unplayable mp4) rather than sealing a corrupt file; an encode failure faults the
  screen source while audio keeps flowing, and `--dump-state` reports per-segment
  frames-consumed vs. samples-written. Live-validated on hardware: a real 5-minute
  capture produced a valid 1080p H.264 mp4 (~1.2 MB vs. ~1.7 GB raw) that decodes
  at 1 fps and lands in a journal by sha256 over the private link.

### Changed

- `tauri.conf.json` bundle icon points at `icons/icon.ico` (was a placeholder PNG),
  so the installer + executable carry the brand mark.
- The WGC screen path now **encodes to H.264 `.mp4`** instead of writing raw RGBA
  frames (see the encoder entry above). The prior raw path was capped to ~1 fps
  purely to bound disk (~2.5 GB per five-minute segment at 1080p); encoding makes a
  segment roughly MB-scale (~1,000× smaller for a quiet screen) and an actual upload
  payload.
- The tray is now built in code with the contract id `tray.root`; the config tray
  block is removed to avoid a duplicate tray resource.
- Incomplete-segment recovery now decides staleness by **rotation boundary**, not
  by file age. The single-instance gate is acquired at boot before any capture
  source starts, so recovery is guaranteed no concurrent writer — there is no live
  writer to race, and the previous age/mtime margin only delayed sealing genuine
  orphans. Recovery now leaves untouched only the one segment whose aligned window
  contains *now* (the engine re-opens and continues it on restart) and seals or
  quarantines every other `.incomplete` immediately. This finalizes a segment the
  prior run had only just crossed out of when it crashed without the former up-to-one-
  window delay; usable captured data is never abandoned. Pure `is_live_segment`
  predicate added to the segment crate; the cross-boundary stale-finalize case is
  covered by tests.

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
