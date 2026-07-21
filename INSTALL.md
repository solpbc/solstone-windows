# INSTALL

Setup for `solstone-windows`. The repo is a cargo workspace whose **pure tier
builds and tests on any host** (Linux/macOS/Windows) — the platform crates'
`windows` dependency is target-gated, so the whole workspace resolves and the
host-testable crates compile without a Windows toolchain. The **binary, webview,
packaging, and FlaUI smoke** require the Windows build box.

## What you need

### Any host (pure tier + contract + tests)

- **Rust 1.96.0**, pinned by `rust-toolchain.toml`. Run `make rust-toolchain`
  to install the exact toolchain, rustfmt, clippy, and Windows MSVC target.
- **cargo-deny 0.20.2** for `make ci` / `make audit`; provision it explicitly with
  `make provision-cargo-deny`.
- That's it for `make test` of the pure crates and `make contract`.

### Windows build box (binary, packaging, smoke)

- The Rust **MSVC** target installed by `make rust-toolchain`.
- **Node.js 24.16.0** and **npm 11.13.0** (for the locked Vite webview build).
- The **WebView2** runtime (evergreen; present on current Windows).
- The **.NET SDK 8.0.422** with net48 targeting support (for the FlaUI harness).
- The Velopack global tool **vpk 1.2.0** for packaging. The full box release-tool
  contract, including MSVC/SDK/PowerShell and signed-mode tools, is pinned in
  `packaging/release-toolchain.json` and observed by `make preflight-release-tools`.

## First build

```bash
# Resolve and build the host-testable crates (works on Linux/macOS/Windows):
cargo build --locked --workspace --exclude solstone-windows-app

# Run the pure-tier tests + the contract drift gate:
make test
```

> On a non-Windows host, the Tauri binary (`solstone-windows-app`) will not build
> — it needs platform webview system libraries. That is expected; exclude it as
> shown above. On the Windows build box it builds against WebView2.

## The full build (Windows build box)

```bash
# Provision the exact committed webview graph (network permitted):
make install

# Build the binary + the webview bundle:
make build
```

`make build` rematerializes the same graph with `npm ci --offline`. To deliberately
update UI dependencies, run `make ui-deps-update`, review `ui/package-lock.json`,
and commit it.

The release contract pins Windows PowerShell 5.1. On the build box, run release
verbs with `PWSH=powershell`, for example `PWSH=powershell make
preflight-release-tools` or `PWSH=powershell make package`; alternatively use the
box-native `scripts/win-package.cmd`.

## The contract

The automation contract is generated, not written:

```bash
make contract        # regenerate automation-contract.json + ui/src/lib/contract.ts
cargo run --locked -q -p xtask -- contract --check   # verify no drift
```

Commit the regenerated files. Never hand-edit them.

## Remote build host (optional)

To drive a Windows build box over SSH from a checkout elsewhere:

```bash
WIN_REMOTE_HOST=<host> make win-host-ci
```

This refuses untracked non-ignored files and an unmerged index, serializes
overlapping runs with a lock, and transfers the tracked working tree by Git
bundle + SCP. It accepts the native gate only when the box's reported HEAD
matches the exact transferred snapshot SHA. Set `WIN_REMOTE_HOST` to the build
box address supplied by your environment.

## Verifying

```bash
make ci    # fmt-check · clippy -D warnings · contract --check · tests · cargo deny
```

`make ci` is the composite gate. On a non-Windows host, `make test` and
`cargo run --locked -q -p xtask -- contract --check` are the focused Rust subset;
the composite gate also runs UI/shell checks and the native Windows box leg.
