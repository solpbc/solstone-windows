# signing (release-artifact code signing)

Release artifacts are signed with the sol pbc code-signing certificate via
Velopack's `--signTemplate` (DigiCert KeyLocker / `smctl`), covering **release
artifacts only** — never source, never intermediate build files (the
certificate's finite signature quota).

- `scripts/package.ps1` builds the `--signTemplate` and signs only when packaged
  with `-Sign` (the release path — set `SOLSTONE_SIGN=1` on the build box and the
  packaging wrapper forwards it). Without it the pack is unsigned, the dev/local
  default, so iterate and delta-update-validation packs don't burn signature quota
  or churn SmartScreen reputation hashes.
- `preflight-auth.ps1` is the credential pre-check that runs before `vpk pack`: it
  fails fast (secret-free) when the signing environment is not provisioned, rather
  than letting the signer fail opaquely mid-pack.
- The signing credentials and the keypair alias are env-supplied — `SM_HOST`,
  `SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`,
  `SM_KEYPAIR_ALIAS` — supplied by the build box, never committed.

Never commit the certificate, its credentials, key material, or `*.pfx` / `*.snk`
/ `*.p12` files. They are git-ignored; keep them out of the repo.
