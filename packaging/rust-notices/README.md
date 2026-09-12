# Rust dependency notices

`RUST_DEPENDENCY_NOTICES.txt` at the repository root reproduces the license
texts of the crates statically linked into `solstone-windows-app` for
`x86_64-pc-windows-msvc`.

Population: non-dev (normal and build) dependency closure from locked Cargo
metadata. That is a conservative over-inclusion, not an exact PE link census.
Workspace crates are AGPL-3.0-only and are covered by `LICENSE`.

`index.json` binds those bytes to `Cargo.lock`. `cargo xtask rust-notices check`
refuses a stale lock or mutated notices file. The release finalizer stages the
notices beside the app and refuses promotion unless the full nupkg
(`lib/app/RUST_DEPENDENCY_NOTICES.txt`) and portable ZIP
(`current/RUST_DEPENDENCY_NOTICES.txt`) match the source bytes. Native release
proof requires the installed copy at
`%LocalAppData%\Solstone\current\RUST_DEPENDENCY_NOTICES.txt` to match.

When `Cargo.lock` changes, regenerate:

```
cargo fetch --locked --target x86_64-pc-windows-msvc
python3 scripts/generate-rust-notices.py
```

`overrides/` holds original upstream license texts for crates that publish no
license file in the crate archive. Do not replace those texts with SPDX
templates. `realfft` 3.5.0 is the one exception: upstream publishes no LICENSE
file at the crate or repository tag, so the override is the MIT text with the
crate's Cargo.toml authors.
