# spikes — reference only

This directory is **excluded from the cargo workspace** (`exclude = ["spikes"]`
in the root `Cargo.toml`), so stale spike code can never break `make ci` — but it
is one `cd` away when implementing the real crate.

> **Import pending.** The spike code itself is not in this scaffold. It is
> imported from the build box in a follow-up step and lands in the directories
> below. Until then these are placeholders that record where each spike goes and
> what it proved.

## Where each spike graduates

| Spike | Lands at | Fact it proved | Production path that supersedes it |
|---|---|---|---|
| GDI screen capture | `spikes/gdi-screen/` | basic screen grab works | superseded by `capture-wgc`; GDI = diagnostic fallback only |
| WASAPI loopback audio | `spikes/wasapi-loopback/` | render-loopback system audio capture | reference for `capture-wasapi` loopback |
| WGC capture | `spikes/wgc/` | Windows.Graphics.Capture frame pump | **direct seed** for `capture-wgc` (production WGC path) |
| eCapture mic | `spikes/mic/` | mic capture + the zero-endpoint (no-device) case | reference for `capture-wasapi` eCapture; informs `NoInputDevice` |
| FlaUI driver (.NET) | **graduates → `harness/driver/`** | FlaUI/UIA3 drives native chrome | becomes the real net48 harness |
| WinForms UI target | `spikes/flaui-scratch/` | FlaUI green against native WinForms | reference only — **no fixture app in production; the installed app is the target** |
| Velopack scratch installer | not committed (build artifact) | per-user silent install shape | informs `packaging/` |

## Rules

- Spikes are reference, not production. Do not depend on them from any workspace
  crate.
- When a spike's fact is reproduced by a tested production crate, the spike stays
  as historical reference; the production path is authoritative.
