# signing (seam — empty by design)

The release-artifact signing seam. **There is nothing to sign yet** — the
validated release path is unsigned (acceptable while SmartScreen is off on the
build box).

When a code-signing certificate is provisioned, signing turns on with **no code
restructure**:

1. `scripts/package.ps1` populates its `$SignTemplate` with the Velopack form:
   `--signTemplate "smctl sign --fingerprint <fp> --input {{file}}"`.
2. Add `preflight-auth.ps1` here as a credential pre-check that runs before
   `vpk pack`.
3. Sign **release artifacts only** — never source, never intermediate build
   files (the certificate's signature-cap discipline).

Never commit the certificate, its credentials, key material, or `*.pfx` /
`*.snk` files. They are git-ignored; keep them out of the repo.
