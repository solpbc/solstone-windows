# Contributing

`solstone-windows` is early public open-source software. Contributions are
reviewed for correctness, safety, privacy, and fit with solstone's
owner-controlled data model.

## Development

```bash
make ci
```

Run `make ci` before asking for review. On a non-Windows host, run the
host-testable subset (`cargo test --workspace --exclude solstone-windows-app`,
`cargo xtask contract --check`).

Use focused commits.

## Pull requests

- Keep changes scoped to one concern.
- Include tests or validation evidence for behavior changes.
- Do **not** add analytics, telemetry, tracking, crash-reporting SDKs, or hosted
  release automation. The privacy denylist in `deny.toml` will reject them.
- Keep `windows` / `windows-rs` in the platform-tier crates only; the pure tier
  must stay host-testable and `#![forbid(unsafe_code)]`.
- If you touch the automation contract, run `make contract` and commit the
  regenerated `automation-contract.json` and `ui/src/lib/contract.ts`.
- Call out capture, segment, lifecycle, packaging, and data-path changes
  explicitly.

## License

By contributing, you agree that your contribution is licensed under AGPL-3.0-only.
