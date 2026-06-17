# The automation contract

Shared protocols are **code, not prose.** Two vocabularies live in one generated,
committed, drift-gated artifact at the repo root: `automation-contract.json`.

## What's in it

1. **AutomationId identifiers** — the namespaced `data-automation-id` / UIA
   AutomationId strings (`tray.menu.start`, `settings.status.appState.state`,
   `about.window.root`, …). Source of truth: `const`s in the `observer-contract`
   crate.
2. **State/source token vocabulary** — the serialized enum tokens
   (`idle`/`observing`/…, `screen`/`system_audio`/…, `active`/`no_input_device`/
   `faulted`/…). **Derived from the `observer-model` enums** via
   `strum::EnumIter`, so you cannot add a `SourceState` variant without the
   contract noticing.

The artifact is deterministic: `_generated` banner first, sorted keys, pretty,
trailing newline.

## Three consumers, one source of truth

- The FlaUI harness reads the JSON to find elements by AutomationId.
- The webview codegen (`ui/src/lib/contract.ts`) embeds the same JSON to stamp
  `data-automation-id`.
- `--dump-state` / `/healthz` emit the token vocabulary.

## The drift gate

- `make contract` (= `cargo xtask contract`) regenerates the JSON + the ui
  codegen; the operator commits the result.
- `cargo xtask contract --check` regenerates in memory and exits 1 on any diff.
- A `#[test] contract_not_stale` shells to `--check`, so `cargo test` alone also
  catches drift.
- `make ci` runs `--check` before the test suite — fail fast.

## How to extend

1. Edit the source of truth (add an AutomationId `const`, or a model enum
   variant).
2. Run `make contract`.
3. Commit the regenerated `automation-contract.json` and `ui/src/lib/contract.ts`.

Never hand-edit the generated files.
