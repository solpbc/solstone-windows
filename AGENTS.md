# solstone-windows

Development guide for the Windows-native solstone observer. This file is the
canonical agent guide; `CLAUDE.md` is a symlink to it.

## 1. Project Overview

`solstone-windows` is the Windows-native solstone observer: per-user,
non-elevated, tray-resident. It gathers screen and system audio — plus the
microphone when one is present — into 5-minute clock-boundary segments on local
disk for the owner's journal. It is a **pairing client, not a journal host**: it
pairs to an existing journal and uploads (a later wave). Public open source.

Keep every visible file clean of private operational context, internal paths,
personal machine names, and unreleasable history. Reference only the public
charter and license.

## 2. Principles

- **KISS / YAGNI.** Wave 1 is a validated skeleton. Stubs are minimal and
  compiling; do not add speculative machinery. The reserved Wave-2 crates are
  named, not built.
- **Honest state, always earned.** Never render `observing` / `ok` unless the
  durable fact is true. `AppPhase::Observing` is *computed* by the reducer, never
  settable. "No microphone input device" is a first-class `SourceState`, not an
  error.
- **Privacy is architecture / data covenant.** No analytics, telemetry, tracking
  SDKs, crash reporters, or phone-home — ever. Enforced by the privacy denylist
  in `deny.toml`. The observer writes local, owner-controlled data; nothing
  leaves the machine except the owner's own upload to their own journal.
- **Quarantine the platform.** Direct `windows` / `windows-rs` use in shipped
  code stays in the audited, target-gated platform-tier crates
  (`capture-screen-encode`, `capture-wgc`, `capture-wasapi`, `platform-win`,
  `pl-transport-win`). The Windows family is forbidden in each strict member's
  shipped (normal+build) graph; dev-only reachability is out of scope because
  dev-dependencies never ship. `xtask` is reviewed Windows-capable build tooling:
  its reparse support and pinned offline jsonschema validator may reach
  `windows-link`, but it never ships. The pure tier carries
  `#![forbid(unsafe_code)]` and is host-testable. Dependency arrows never point
  pure → platform.
- **No GitHub Actions release path.** Releases are operator-driven, by hand, from
  a known build box via local `make`. `.github/workflows/` does not exist, by
  policy, permanently.
- **Agent-native CLI surface.** `--dump-state` / `/healthz` expose the honest
  state; `--check-update` readies an update headlessly (check + download + stage)
  and `--apply-update` installs the staged one (the CLI analogs of the in-app
  check / relaunch-to-install); atomic `make` verbs wrap every multi-step
  operation. Never hand-chain `cargo build` → `vpk pack` or any publication
  transport — invoke the packaging verb; release publication belongs to the aggregate provenance publisher.
- **Shared protocols are code.** The AutomationId identifiers and the
  health/state token vocabulary are a generated, committed,
  drift-gated `automation-contract.json` — not prose. The source of truth is the
  `observer-contract` crate; the state-token vocabulary derives from the
  `observer-model` enums.
- **Per-user, single-process.** `%LocalAppData%`, no UAC, one per-session named
  mutex. Capture runs in-process for Wave 1; the separate capture-worker split is
  a deferred, named escape hatch — do not build it without a soak-instability
  reason.

## 3. Commands

| Verb | Does |
|---|---|
| `make rust-toolchain` | idempotently install the exact pinned Rust toolchain, rustfmt, clippy, and Windows MSVC target |
| `make build` | `cargo build` the binary + `npm run build` the webview → `ui/dist` |
| `make test` | `cargo test --workspace` (the pure tier runs off-Windows too) |
| `make ci` | host fmt/clippy/contract/tests · offline locked bans/licenses/sources · UI/shell tests · native Windows build/test |
| `make audit` | refresh the RustSec database, then check advisories against the locked graph |
| `make contract` | regenerate `automation-contract.json` + the ui codegen; commit the result |
| `make check-observer-contract` | offline local structural/behavioral verification of the pinned observer-client authority bundle |
| `make check-rust-release-manifest` | offline schema, checkout binding, ledger, current-bundle, and deterministic-render verification; mode selected by `MANIFEST` or `RELEASE_DIR` |
| `make package` | pinned release-tool preflight → metadata version gate → tracked-lock guard → offline UI install/build → locked release build → Velopack pack → `Releases/` (unsigned; `-Sign` / `SOLSTONE_SIGN=1` signs a release) |
| `make publish-r2` | fail-closed direct-publication guard; R2 publication belongs to the aggregate provenance publisher |
| `make pull-releases` | pull the box's packed `Releases/` for a controlled aggregate workflow; does not publish |
| `make publish` | fail-closed direct-publication guard; GitHub publication belongs to the aggregate provenance publisher |
| `make smoke` | Session-1 scheduled-task FlaUI smoke vs the installed app |
| `make run` | launch from the tree + tail `%LocalAppData%\Solstone\logs\` |
| `make clean` | `cargo clean` + remove `ui/dist` and `Releases/` |

### Target evidence

| Repository entry point | Evidence class | Exact claim |
|---|---|---|
| `make test`, `make ui-test`, `make test-scripts`, and the local Rust legs of `make ci` | Host evidence | Linux-host formatting, compilation/tests for the host-testable subset, UI tests, and shell policy; no Windows compilation |
| `make purity-check` | Cross-target classification evidence | Enumerates every workspace member from `cargo metadata` and inspects each exactly once with `cargo tree --target all --all-features -e normal,build`; the Windows family is forbidden in each strict member's shipped (normal+build) graph. Dev-only reachability is out of scope because dev-dependencies never ship; the reviewed Windows-capable set includes platform/composition/app members and `xtask` build tooling. Unknown or stale exceptions fail. This does not compile or link MSVC code |
| `make check-rust-release-manifest` | Host evidence | Offline exact-schema and semantic self-check with no environment selector; `MANIFEST=<path>` verifies one manifest and its named sibling bytes without claiming completeness; `RELEASE_DIR=<path>` classifies one exact flat current-only bundle |
| `make win-host-ci` → `scripts/win-ci.cmd` | Native-target evidence | Windows build/test for the workspace excluding the app, plus contract and purity checks; the caller verifies that the box built the exact transferred snapshot by matching its reported HEAD to the intended snapshot SHA; no app package, install, sign, or smoke |
| `scripts/win-app-build.cmd` | Native app-build evidence | Builds the UI and Windows app binary; no package, install, sign, or smoke |
| `make package` / `scripts/win-package.cmd` | Package-construction evidence | Release app build plus Velopack pack; unsigned unless signing is explicitly enabled; no install or smoke |
| `SOLSTONE_SIGN=1 scripts/win-package.cmd` → install emitted setup → `make smoke` | Shipped-artifact proof | Exercises the installed signed bytes in the interactive Windows session |

Linux has no compiling cross-target MSVC check because it cannot link the
Windows MSVC target. `make ci` is a composite gate and still needs npm plus
`WIN_REMOTE_HOST`; it is not an offline gate. Only its cargo-deny
bans/licenses/sources sub-gate is offline.

All gated project dependency resolution holds `Cargo.lock` with `--locked`. The
one deliberate dependency-update path is `cargo update -p <crate>`: review and
commit the resulting `Cargo.lock`, then rerun `make ci` and `make audit`.

The workspace lint floor denies unsafe code. Item-level exceptions may exist only
inside the five audited platform crates named above; crate-wide
`#![allow(unsafe_code)]` is forbidden.

**Off-Windows dev host:** the Rust-MSVC / windows-rs / Tauri toolchain only builds
on Windows, so on a non-Windows dev host run the gate on the Windows build box with
`WIN_REMOTE_HOST=user@host make win-host-ci`. It refuses untracked non-ignored
files and an unmerged index, then snapshots the exact committed, staged, and
unstaged tracked working tree into a uniquely named, verified bundle on the
CAS-guarded stable `refs/heads/__swsync` ref. A common-directory flock serializes
overlapping runs; `flock` is required on the Linux driver host, with no unlocked
fallback. The caller ships the bundle over SSH as `swbuild.bundle` (git bundle +
scp — no rsync); the box bootstrap hard-checks it out under `~/swbuild` and runs
`scripts/win-ci.cmd`. The caller accepts the result only when the box's checked-out
HEAD equals the exact transferred snapshot SHA and `WIN_CI_OK` is present.
`WIN_REMOTE_HOST` is supplied by your environment, never committed. The live FlaUI
smoke + lifecycle matrix stay operator-direct on the box (not part of
`win-host-ci`).

Build-box gotchas: the FlaUI harness targets **net48** and needs
`Accessibility.dll` in the publish layout; invoke `.cmd` shims via `cmd.exe /c`;
use explicit tool paths until `PATH` refreshes after a package install; the smoke
runs via a scheduled task into Session 1. **Keep `.ps1` scripts ASCII-only** —
Windows PowerShell 5.1 (the box default shell) reads a non-BOM `.ps1` in the
system codepage, so a UTF-8 em-dash or smart-quote corrupts and can break string
parsing (e.g. an em-dash inside a `throw "..."` aborts `vpk pack`). Use plain `-`
and `'`/`"` in scripts that run on the box.

## 4. Source Layout

```text
crates/
  ── pure tier ── (no `windows` in the shipped normal+build graph; #![forbid(unsafe_code)]; host-testable)
  observer-model/      shared vocabulary + the three source traits + HealthDump
  observer-segment/    5-min clock-boundary rotation + segment-key math
  observer-state/      honest reducer — Observing is computed, never settable
  observer-health/     --dump-state / /healthz serialization of HealthDump
  observer-recovery/   incomplete-segment scan/finalize over a RecoveryFs trait
  observer-lifecycle/  backoff + circuit-breaker state machine
  observer-contract/   AutomationId source of truth + the JSON generator
  observer-pl/         pair-link parse + spl framing + observer wire + multipart + CA-fp pin (pure)
  observer-audio/      combine + downmix + resample + FLAC-encode segment audio
  ── platform tier ── (windows-rs quarantine, target-gated; unsafe only here)
  capture-wgc/         Windows.Graphics.Capture screen source
  capture-wasapi/      WASAPI loopback system audio + eCapture mic (owns NoInputDevice)
  platform-win/        session/power pump, named-mutex, %LocalAppData%, fs impls
  pl-transport-win/    framed-mTLS transport (rustls) + observer client + upload coordinator + heartbeat
  ── composition tier ──
  capture-engine/      orchestrator: sources→writer→rotation→state→recovery
src-tauri/             the binary: tray + Settings/About + IPC + arg dispatch
ui/                    WebView2 front-end (vanilla TS + Vite); pure renderer
xtask/                 cargo xtask: contract [--check], purity-check, version-gate, package, dev
contracts/observer-client/ test-only vendored authority bytes + checked consumer adoption metadata
harness/               net48 FlaUI/UIA smoke driver (not a cargo member)
packaging/             Velopack config + hooks/ + signing/ seam
scripts/               PowerShell impls behind the make verbs
docs/                  architecture / contract / runbook / lifecycle
spikes/                reference-only, excluded from the workspace build
```

The **DAG rule**: dependency arrows never point pure → platform. The pure tier
holds all the logic worth testing; the platform tier holds the `unsafe`
WinRT/COM seams.

**`pl-transport-win` keeps its transport core rustls-based and host-testable; its
only `windows-rs` / `unsafe` use is the target-gated DPAPI credential-wrap at rest**.
It sits in the platform tier because it owns the OS-adjacent transport (sockets,
TLS, the network seam), but it compiles and tests on the Linux dev host too.
That is deliberate: the live cross-repo pair+ingest gate can run off-Windows
against a journal on the dev box (see `crates/pl-transport-win/examples/live_gate.rs`),
and the box's `win-host-ci` still builds + tests it on real MSVC alongside the
windows-rs tier.

## 5. The contract

The AutomationId identifiers and the health/state token vocabulary live in one
generated artifact, `automation-contract.json`, at the repo root. Source of
truth: the `observer-contract` crate (AutomationId `const`s + the generator); the
token vocabulary derives from the `observer-model` enums via `strum::EnumIter`.

- `make contract` regenerates the JSON + `ui/src/lib/contract.ts`; commit both.
- `cargo xtask contract --check` (run by `make ci` and by the `contract_not_stale`
  test) exits 1 on drift, so `cargo test` alone also catches it.
- Three consumers: the FlaUI harness (finds elements), the webview codegen
  (stamps `data-automation-id`), and `--dump-state` / `/healthz` (the tokens).

To extend: edit the source of truth, run `make contract`, commit the regenerated
files. Never hand-edit the generated files.

The FlaUI smoke asserts on the **health dump** (Tier 0) and **native chrome**
(Tier 1); the webview `data-automation-id` is best-effort (Tier 2) and the green
path must not depend on Chromium UIA resolving.

### Observer-client authority bundle

The language-neutral Journal observer-client contract is vendored as immutable,
test-only bytes under `contracts/observer-client/bundle/`. Consumer adoption
metadata lives beside, not inside, the bundle. The offline verifier and
authority-derived conformance tests do not enter the production dependency or
packaging graph. This authority bundle is distinct from the generated
AutomationId/state-token contract above.

See `docs/observer-contract-adoption.md`.

## 6. Lifecycle

- **Production launch:** a per-user login item into interactive Session 1 — a
  single named value under the `HKCU\…\CurrentVersion\Run` key (no admin, no
  machine-wide `HKLM`, no scheduled task). It is *ensured idempotently on every
  launch* (write-only-when-missing-or-stale, so it self-heals and never
  duplicates) by `platform_win::autostart`, not tied to a one-shot install
  signal; the Velopack uninstall hook removes it. **Test launch:** low-privilege
  scheduled task (`LogonType=Interactive`) into Session 1 (FlaUI smoke only).
  Never conflate them.
- **Handlers:** lock/unlock pause+resume; display-change re-acquires the screen
  source; power suspend/resume pause+resume. A required-source fault drops out of
  `observing` into `Error` via the backoff/breaker.
- **Single instance:** a per-session named mutex; a second launch surfaces
  Settings on the first and exits.
- **Velopack hooks** the app must handle: `--veloapp-install`, `--veloapp-updated`,
  `--veloapp-obsolete`, `--veloapp-uninstall` (the uninstall fast-callback removes
  the autostart login item). The app must be Velopack-aware so the hooks exit 0.

See `docs/lifecycle-matrix.md` for the full table.

## 7. Packaging & release

Velopack, per-user `%LocalAppData%`, no UAC. The **primary update feed is R2** at
`updates.solstone.app/solstone-windows/` — a privacy-clean,
no-analytics static surface, so each user's scheduled update check stays a
bare first-party manifest GET on our own surface with **no query string** (no app
version, no app id, no per-user identifier) rather than hitting a third party.
(The updater neutralizes Velopack's per-install staging id — see
`src-tauri/src/update.rs`.) R2 is the authoritative update feed. A GitHub
Releases mirror (a tagged `v<version>` release with the artifacts + the
`CHANGELOG.md ## [<version>]` notes attached) is optional and non-authoritative;
its success cannot gate authoritative publication, update delivery, or release
evidence. Direct R2, GitHub, winget, and scoop publication entry points are
fail-closed: release publication belongs to the aggregate provenance publisher.
That future component publishes each finalized signed release to R2 as the
authoritative feed and may optionally mirror it to GitHub. No GitHub mirror is
required, and a missing or failed mirror never blocks a release.
The in-app updater fetches `releases.win.json` via a query-free first-party
manifest GET (a small custom Velopack `UpdateSource`); package downloads still
request the package files by filename from the same first-party feed host.
Release artifacts are signed (DigiCert
KeyLocker via Velopack's `--signTemplate`); signing is opt-in and release-only
(`-Sign` / `SOLSTONE_SIGN=1`) so dev/local packs stay unsigned, and the
credentials are env-supplied, never committed. Signing covers release artifacts
only. Package construction performs no publication auth or transport; release
publication belongs to the aggregate provenance publisher. See `docs/release-runbook.md`.

The offline Rust release-manifest verifier has three modes. With no selector it
runs only committed fixtures and deterministic rendering. `MANIFEST=<path>`
checks that manifest plus every exact named sibling but is not a complete or
publishable-directory classification. `RELEASE_DIR=<path>` requires a flat
current-only directory containing the companion
`solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json` and exactly
these six manifest-listed files: `assets.win.json`, `RELEASES`,
`releases.win.json`, `Solstone-<VERSION>-full.nupkg`,
`solstone-setup-<VERSION>.exe`, and `Solstone-win-Portable.zip`; when the current
feeds advertise a delta, `Solstone-<VERSION>-delta.nupkg` is required too. The
companion is never self-listed, so a complete bundle has seven files, or eight
with the current delta. This verifier does not package, sign, authenticate, or
publish, and all direct publication entry points remain fail-closed.

## 8. Safety Rails

- No telemetry, analytics, tracking, crash reporting, or phone-home. The privacy
  denylist in `deny.toml` enforces it at the dependency-graph level.
- Never render unearned state. The reducer computes `Observing`; nothing else may
  set it.
- No `.github/workflows/`. Releases are operator-driven, by hand.
- Do not commit secrets, keystores, signing credentials, certificates, captured
  media, or screenshots.
- Do not make the webview UIA tree load-bearing for the smoke.
- Do not split the capture worker into a separate process without a documented
  soak-instability reason.

## 9. Two-register brand voice

- The app is **sol** (in UI copy the app calls itself "sol"); the memory is
  **your journal**; **solstone** is the platform/family (store listing, platform
  references, domains); **sol pbc** is the company. Brand names are lowercase
  always.
- "observer/observers/observe/observing" is engineering-internal vocabulary that
  NEVER appears in user-visible copy.
- Approved sol-subject verbs (owner-visible): lives · experiences (…your day /
  …with you) · takes in what you take in · keeps · remembers · tends · notices.
  Plain mechanism verbs (uses, connects, syncs, falls back, recognizes, needs)
  are fine.
- Never (user-visible): bare "sol listens/hears" (mirrored "sol hears what you
  hear" is ok); "sol observes/watches/sees/captures/records/monitors/tracks";
  "keeper" as a title (say "sol keeps your journal", a verb); "meet sol"; "sol
  agent".
- Banned surveillance verbs describing the app in owner-visible copy: watch,
  capture, record, monitor, track, collect.
- **Code identifiers keep technical terms verbatim** (`capture-engine`,
  `ScreenSource`, the `capture-wgc` crate) — `capture`/`observer` are fine in
  code, never in owner-visible copy.

## 10. SPDX Source Headers

New `.rs`, `.ts`, `.ps1`, and `.cs` source files carry:

```text
SPDX-License-Identifier: AGPL-3.0-only
Copyright (c) 2026 sol pbc
```

Use the comment syntax native to the file type. Do **not** add headers to
generated files, scaffolding configuration, or docs.

## 11. License

AGPL-3.0-only.
