# Vendored: the native-browser contract

The browser extension's native-messaging contract and its shared Rust framing
crate, copied byte for byte from
[`solstone-browser`](https://github.com/solpbc/solstone-browser) at
`731a80ddfed3a5e80bf8ce658cc71f761fa75f83`:

- `contracts/native-browser/`: the envelope schema, authority, corpus, recipes
  and registration table;
- `crates/native-browser-frame/`: framing, the decoder and encoder, timing
  predicates and the registration renderer, with its own tests.

`adoption.json` pins the bundle (same shape as the macOS app's adoption pin), and
`vendored-files.sha256` lists every vendored file's upstream hash.
`crates/browser-host/tests/contract.rs` checks both, runs every corpus vector
through the shared decoder and rebuilds every frame recipe byte for byte, so
the Windows gate exercises the same vectors as every other app. (The crate's
own test files stay upstream's; this repository's source-policy scan does not
follow a module outside a member.)

**One local change:** `crates/native-browser-frame/Cargo.toml` declares
`rust-version = "1.96.0"` (upstream `1.97.1`) so it builds on this repository's
pinned toolchain; the crate's code builds and passes its tests on 1.96.0
unchanged. It is not listed in `vendored-files.sha256` for that reason.

The directory sits outside the workspace members: upstream's formatting and
lints are upstream's, and are not re-judged by this repository's gate.

To update: replace both directories from the new upstream revision, regenerate
`vendored-files.sha256` (`find contracts crates -type f ! -name Cargo.toml |
sort | xargs sha256sum`), update `adoption.json`, and re-apply the
`rust-version` line.
