# INSTALL

Setup for `solstone-windows`. The repo is a cargo workspace whose **pure tier
builds and tests on any host** (Linux/macOS/Windows) — the platform crates'
`windows` dependency is target-gated, so the whole workspace resolves and the
host-testable crates compile without a Windows toolchain. The **binary, webview,
packaging, and FlaUI smoke** require the Windows build box.

## What you need

### Any host (pure tier + contract + tests)

- **Rust** (stable, 1.82+). `cargo --version` should print 1.82 or newer.
- That's it for `make test` of the pure crates and `make contract`.

### Windows build box (binary, packaging, smoke)

- The Rust **MSVC** toolchain: `stable-x86_64-pc-windows-msvc` (the
  `rust-toolchain.toml` pin). `rustup target add x86_64-pc-windows-msvc` if needed.
- **Node.js 18+** and npm (for the Vite webview build).
- The **WebView2** runtime (evergreen; present on current Windows).
- The **.NET SDK** with net48 targeting support (for the FlaUI harness).
- **Velopack** (`vpk`) and the **GitHub CLI** (`gh`) for packaging/publishing.

## First build

```bash
# Resolve and build the host-testable crates (works on Linux/macOS/Windows):
cargo build --workspace --exclude solstone-windows-app

# Run the pure-tier tests + the contract drift gate:
make test
```

> On a non-Windows host, the Tauri binary (`solstone-windows-app`) will not build
> — it needs platform webview system libraries. That is expected; exclude it as
> shown above. On the Windows build box it builds against WebView2.

## The full build (Windows build box)

```bash
# Install the webview deps once:
npm --prefix ui install

# Build the binary + the webview bundle:
make build
```

## The contract

The automation contract is generated, not written:

```bash
make contract        # regenerate automation-contract.json + ui/src/lib/contract.ts
cargo xtask contract --check   # verify no drift (also runs in `make ci` and tests)
```

Commit the regenerated files. Never hand-edit them.

## Remote build host (optional)

To drive a Windows build box over SSH from a checkout elsewhere:

```bash
WIN_REMOTE_HOST=<host> make win-host-ci
```

This syncs the tree to a dedicated remote directory (`rsync --delete`) and runs
`make ci` there. Set `WIN_REMOTE_HOST` to your build box's hostname.

## Verifying

```bash
make ci    # fmt-check · clippy -D warnings · contract --check · tests · cargo deny
```

`make ci` is the gate. On a non-Windows host, run the subset that applies
(`cargo test --workspace --exclude solstone-windows-app`, `cargo xtask contract
--check`); the full gate including the binary and `cargo deny` runs on the build
box (and `cargo deny` requires `cargo install cargo-deny`).
