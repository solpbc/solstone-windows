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
- **Quarantine the platform.** `windows` / `windows-rs` may appear **only** in
  the platform-tier crates (`capture-wgc`, `capture-wasapi`, `platform-win`), and
  is declared target-gated there. The pure tier carries `#![forbid(unsafe_code)]`
  and is host-testable. Dependency arrows never point pure → platform.
- **No GitHub Actions release path.** Releases are operator-driven, by hand, from
  a known build box via local `make`. `.github/workflows/` does not exist, by
  policy, permanently.
- **Agent-native CLI surface.** `--dump-state` / `/healthz` expose the honest
  state; `--check-update` readies an update headlessly (check + download + stage)
  and `--apply-update` installs the staged one (the CLI analogs of the in-app
  check / relaunch-to-install); atomic `make` verbs wrap every multi-step
  operation. Never hand-chain `cargo build` → `vpk pack` → `gh release` — invoke
  the verb.
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
| `make build` | `cargo build` the binary + `npm run build` the webview → `ui/dist` |
| `make test` | `cargo test --workspace` (the pure tier runs off-Windows too) |
| `make ci` | fmt-check · clippy `-D warnings` · contract `--check` · tests · `cargo deny check` |
| `make contract` | regenerate `automation-contract.json` + the ui codegen; commit the result |
| `make package` | `make build` → Velopack pack → `Releases/` (unsigned; `-Sign` / `SOLSTONE_SIGN=1` signs a release) |
| `make publish-r2` | upload `Releases/` to the R2 update feed (`updates.solstone.app/solstone-windows/`, feed-last) — the **primary** auto-update channel; run on the release host |
| `make pull-releases` | pull the box's packed `Releases/` to the release host (before `publish-r2`) |
| `make publish` | upload `Releases/` to GitHub Releases — the demoted source-hygiene mirror |
| `make smoke` | Session-1 scheduled-task FlaUI smoke vs the installed app |
| `make run` | launch from the tree + tail `%LocalAppData%\Solstone\logs\` |
| `make clean` | `cargo clean` + remove `ui/dist` and `Releases/` |

**Off-Windows dev host:** the Rust-MSVC / windows-rs / Tauri toolchain only builds
on Windows, so on a non-Windows dev host run the gate on the Windows build box with
`WIN_REMOTE_HOST=user@host make win-host-ci`. It bundles your exact working tree
(committed or not), ships it over SSH (git bundle + scp — no rsync), and runs build
+ tests + the contract check on the box, streaming results back. `WIN_REMOTE_HOST`
is supplied by your environment, never committed. The live FlaUI smoke + lifecycle
matrix stay operator-direct on the box (not part of `win-host-ci`).

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
  ── pure tier ── (no `windows` dep; #![forbid(unsafe_code)]; host-testable)
  observer-model/      shared vocabulary + the three source traits + HealthDump
  observer-segment/    5-min clock-boundary rotation + segment-key math
  observer-state/      honest reducer — Observing is computed, never settable
  observer-health/     --dump-state / /healthz serialization of HealthDump
  observer-recovery/   incomplete-segment scan/finalize over a RecoveryFs trait
  observer-lifecycle/  backoff + circuit-breaker state machine
  observer-contract/   AutomationId source of truth + the JSON generator
  observer-pl/         pair-link parse + spl framing + observer wire + multipart + CA-fp pin (pure)
  ── platform tier ── (windows-rs quarantine, target-gated; unsafe only here)
  capture-wgc/         Windows.Graphics.Capture screen source
  capture-wasapi/      WASAPI loopback system audio + eCapture mic (owns NoInputDevice)
  platform-win/        session/power pump, named-mutex, %LocalAppData%, fs impls
  pl-transport-win/    framed-mTLS transport (rustls) + observer client + upload coordinator + heartbeat
  ── composition tier ──
  capture-engine/      orchestrator: sources→writer→rotation→state→recovery
src-tauri/             the binary: tray + Settings/About + IPC + arg dispatch
ui/                    WebView2 front-end (vanilla TS + Vite); pure renderer
xtask/                 cargo xtask: contract [--check], package, dev
harness/               net48 FlaUI/UIA smoke driver (not a cargo member)
packaging/             Velopack config + hooks/ + signing/ seam
scripts/               PowerShell impls behind the make verbs
docs/                  architecture / contract / runbook / lifecycle
spikes/                reference-only, excluded from the workspace build
```

The **DAG rule**: dependency arrows never point pure → platform. The pure tier
holds all the logic worth testing; the platform tier holds the `unsafe`
WinRT/COM seams.

**`pl-transport-win` is the one platform-tier crate with no `windows-rs` and no
`unsafe`** — it is built on rustls (ring), which is cross-platform. It sits in the
platform tier because it owns the OS-adjacent transport (sockets, TLS, the network
seam), but it compiles and tests on the Linux dev host too. That is deliberate: the
live cross-repo pair+ingest gate can run off-Windows against a journal on the dev
box (see `crates/pl-transport-win/examples/live_gate.rs`), and the box's
`win-host-ci` still builds + tests it on real MSVC alongside the windows-rs tier.

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
`updates.solstone.app/solstone-windows/` (`make publish-r2`) — a privacy-clean,
no-analytics static surface, so each user's scheduled update check stays a
bare first-party manifest GET on our own surface with **no query string** (no app
version, no app id, no per-user identifier) rather than hitting a third party.
(The updater neutralizes Velopack's per-install staging id — see
`src-tauri/src/update.rs`.) GitHub Releases is demoted to a source-hygiene
**mirror** (`make publish`, a tagged source release with the artifacts attached).
The in-app updater fetches `releases.win.json` via a query-free first-party
manifest GET (a small custom Velopack `UpdateSource`); package downloads still
request the package files by filename from the same first-party feed host.
Release artifacts are signed (DigiCert
KeyLocker via Velopack's `--signTemplate`); signing is opt-in and release-only
(`-Sign` / `SOLSTONE_SIGN=1`) so dev/local packs stay unsigned, and the
credentials are env-supplied, never committed. Signing covers release artifacts
only. The R2 upload runs on the release host (where the cloud auth lives), not the
signing box. See `docs/release-runbook.md`.

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

- **Owner-facing copy** uses "observers + journal" and "sol the keeper". The
  brand is lowercase in copy.
- **Banned surveillance verbs in owner-visible copy:** `watch`, `capture`,
  `record`, `monitor`, `track`, `collect`. Use "observe" / "gather" / "the
  journal receives".
- **Code identifiers keep technical terms verbatim** — `capture-engine`,
  `ScreenSource`, the `capture-wgc` crate, etc. `capture` is fine in code; not in
  owner-visible copy.

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
