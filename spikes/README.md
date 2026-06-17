# spikes — reference only

This directory is **excluded from the cargo workspace** (`exclude = ["spikes"]`
in the root `Cargo.toml`), so stale spike code can never break `make ci` — but it
is one `cd` away when implementing the real crate.

The plumbing spikes that proved the Windows observer stack are **imported here as
reference**. Each is the source as it was spiked on the build box, trimmed to the
source itself (build artifacts, published binaries, and box-operational driver
scripts stayed on the box). They are reference, not production: do not depend on
them from any workspace crate.

## What landed (and what supersedes it)

| Spike | Here | Fact it proved | Production path |
|---|---|---|---|
| GDI screen capture | `spikes/gdi-screen/` | headless screen grab via GDI BitBlt → PNG | superseded by `capture-wgc`; GDI = diagnostic fallback only |
| WASAPI loopback audio | `spikes/wasapi-loopback/` | render-loopback system-audio capture (frame + non-zero-byte counts) | reference for `capture-wasapi` loopback |
| WGC capture | `spikes/wgc/` | Windows.Graphics.Capture frame pump (`windows-capture`) | **direct seed** for `capture-wgc` (production WGC path) |
| eCapture mic | `spikes/mic/` | mic endpoint enumeration + the **zero-active-endpoint** (no-device) case | reference for `capture-wasapi` eCapture; informs `SourceState::NoInputDevice` |
| WinForms UI target | `spikes/flaui-scratch/` | FlaUI green against native WinForms chrome | reference only — **no fixture app in production; the installed app is the target** |
| FlaUI driver (.NET) | `spikes/flaui-driver/` | FlaUI/UIA3 drives native chrome from net48 (with `Accessibility.dll` in the publish layout) | **graduates → `harness/driver/`** in the Wave-1 FlaUI smoke work (1D); kept here as the proven reference |

### Notes on the import

- **gdi-screen + wasapi-loopback** were two bins of one box-side project
  (`capture-spike`); **wgc + mic** were two bins of another
  (`observer-plumbing-spikes`). Each is split here into a self-contained reference
  package. The per-spike `Cargo.toml` reproduces its source project's dependency
  block **verbatim**, so a `windows` feature set may be the superset shared with
  its sibling bin (each `Cargo.toml` header says so).
- **flaui-driver is reference, not yet graduated.** §5 of the repo-init plan has
  the `ui-driver` spike graduating into `harness/driver/` as the real net48
  harness — but that is Wave-1 (1D) harness work. The bootstrap `harness/driver/`
  skeleton remains the graduation home; `harness/README.md` already points there.
  The proven driver source lives here until 1D graduates it.
- **Not imported (intentionally):** the box-side driver/probe scripts (operational
  scaffolding tied to box-local paths), the Velopack scratch installer + its input
  exe (build artifacts), and every `target/`, `bin/`, `obj/`, and `publish/` tree.

## Rules

- Spikes are reference, not production. Do not depend on them from any workspace
  crate.
- When a spike's fact is reproduced by a tested production crate, the spike stays
  as historical reference; the production path is authoritative.
