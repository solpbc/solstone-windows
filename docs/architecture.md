# Architecture

A single cargo workspace at the repo root. The observer binary (`src-tauri/`) is
a workspace *member*, not the root, so the capture engine and its pure logic
compile and test without Tauri — and the pure tier runs on any host with no
Windows toolchain.

## Three crate tiers

`windows-rs` is a **quarantine, not a dependency.** Every crate that touches
`windows` is a thin shim implementing a trait defined in a pure crate. Dependency
arrows never point pure → platform.

| Tier | Crates | Rule |
|---|---|---|
| **Pure** | `observer-{model,segment,state,health,recovery,lifecycle,contract}` | no `windows` dep; `#![forbid(unsafe_code)]`; host-testable |
| **Platform** | `capture-wgc`, `capture-wasapi`, `platform-win` | `windows-rs` (target-gated); `unsafe` isolated here |
| **Composition** | `capture-engine` (lib), `src-tauri` (bin) | inject concrete platform impls into the trait seams |

The crate boundary is the only mechanically-enforceable purity boundary in Rust:
a `windows` dep in a pure crate shows up immediately as a new edge in the
dependency graph. The payoff is that the bulk of the logic — rotation math, state
transitions, recovery scans, contract codegen — is tested off-Windows.

### What each crate does

- **observer-model** — shared vocabulary: `AppPhase`, `SourceKind`,
  `SourceState` (incl. the first-class `NoInputDevice`), `PauseReason`,
  `ErrorReason`, `SegmentKey`, the three source traits, and the `HealthDump`
  honest-state payload.
- **observer-segment** — 5-minute clock-boundary rotation and segment-key math
  over a `SegmentFs` seam; property-tested with a synthetic clock.
- **observer-state** — the honest app-state reducer. `Observing` is *computed*
  from real source state, never settable.
- **observer-health** — one JSON encoding of `HealthDump` for `--dump-state`,
  `/healthz`, and the `health://changed` event.
- **observer-recovery** — incomplete-segment scan/finalize over a `RecoveryFs`
  seam.
- **observer-lifecycle** — backoff + circuit-breaker restart policy; fake-clock
  tested.
- **observer-contract** — AutomationId source of truth + the deterministic
  `automation-contract.json` generator; the state-token vocabulary derives from
  the model enums via `strum::EnumIter`.
- **capture-wgc** — Windows.Graphics.Capture screen source.
- **capture-wasapi** — WASAPI render-loopback system audio + eCapture mic; owns
  the `NoInputDevice` determination.
- **platform-win** — session/power notification pump, per-session named-mutex
  single-instance gate, `%LocalAppData%` paths, the real `SegmentFs`/`RecoveryFs`.
- **capture-engine** — composition-tier orchestrator: sources → writer →
  rotation → state → recovery. Tauri-agnostic; host-testable with fake sources.

## Capture is in-process (Wave 1)

`capture-engine` is a library crate the shell depends on, run on a background
task in the same process. The separate capture-worker process is a **named,
deferred escape hatch** — only if a soak shows WebView2 instability. The clean
crate seam makes that flip a re-host of one crate, not a rewrite.

## Reserved (Wave 2, not created at bootstrap)

- `observer-pl` — pair-link parse + framing + observer wire protocol (pure).
- `pl-transport-win` — framed-mTLS transport (rustls default).

These are named here and created when Wave 2 starts; the bootstrap does not ship
empty Wave-2 crates.
