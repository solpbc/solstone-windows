# signing (release-artifact code signing)

Release artifacts are signed with the sol pbc code-signing certificate via
Velopack's `--signTemplate` (DigiCert KeyLocker / `smctl`), covering **release
artifacts only** — never source, never intermediate build files (the
certificate's finite signature quota).

- `scripts/package.ps1` is the thin preflight/version/lock wrapper that delegates
  once to the xtask finalizer. With `-Sign` (or exactly `SOLSTONE_SIGN=1`) the
  xtask transaction supplies the resolver-selected `--signTemplate` and verifies
  the final setup with selected SignTool. Without it the pack is unsigned, the
  dev/local default, so iteration packs do not burn signature quota or churn
  SmartScreen reputation hashes.
- `../preflight-release-tools.ps1` is the non-credential, network-free tool check.
  In signed mode it selects exact smctl and SignTool identities without signing or
  verification.
- `preflight-auth.ps1 -SmctlPath <selected-absolute-path>` is the credential
  pre-check that runs before `vpk pack`: it fails fast (secret-free) when the
  signing environment is not provisioned and never resolves ambient smctl.
- The signing credentials and the keypair alias are env-supplied — `SM_HOST`,
  `SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`,
  `SM_KEYPAIR_ALIAS` — supplied by the build box, never committed.

Never commit the certificate, its credentials, key material, or `*.pfx` / `*.snk`
/ `*.p12` files. They are git-ignored; keep them out of the repo.
