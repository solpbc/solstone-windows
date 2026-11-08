# signing (release-artifact code signing)

Release artifacts are signed two ways, and `packaging/signing-policy.json` names
which signatures each file must carry:

- **Every executable Velopack signs except the setup** (the app, its execution
  stub and the updater, inside the full nupkg and the portable ZIP) carries one
  Microsoft Artifact Signing signature and nothing else. Velopack's
  `--signTemplate` runs `artifact-sign.ps1 -Mode replace` on each.
- **The setup** keeps the DigiCert KeyLocker certificate (`smctl`) as its
  primary signature, with one nested Artifact signature after it. After Velopack's output is reconciled to
  `solstone-setup-<version>.exe`, the finalizer strips the Artifact signature
  Velopack gave it (`-Mode remove`), signs it once with `smctl`, then appends the
  nested signature (`-Mode append`).

Signing covers **release artifacts only**, never source and never intermediate
build files: DigiCert charges per signature.

- `scripts/package.ps1` is the thin preflight/version/lock wrapper that delegates
  once to the xtask finalizer. With `-Sign` (or exactly `SOLSTONE_SIGN=1`) the
  xtask transaction supplies the resolver-selected `--signTemplate`, signs the
  setup, and verifies the setup and every packaged executable with the selected
  SignTool. Without it the pack is unsigned, the dev/local default, so iteration
  packs do not spend signatures or churn SmartScreen reputation hashes.
- `../preflight-release-tools.ps1` is the non-credential, network-free tool check.
  In signed mode it selects exact smctl and SignTool identities and the Artifact
  actions without signing or verification.
- `preflight-auth.ps1 -SmctlPath <selected-absolute-path>` is the credential
  pre-check that runs before `vpk pack`: it fails fast (secret-free) when the
  DigiCert signing environment is not provisioned and never resolves ambient smctl.
- `artifact-sign.ps1 -Mode replace|append|remove -Path <file>` is the one
  Artifact Signing entry point. It runs jsign 7.6 on a portable Java runtime at
  `C:\Tools\jsign-7.6` on the build box, each pinned by SHA-256, and fails if
  either is missing or differs, if jsign fails, or if the file is unchanged
  afterwards. `replace` and `append` need `SOLSTONE_NESTED_SIGNING_TOKEN`, a
  short-lived signing-profile token supplied per release; `remove` needs none.
- The DigiCert credentials and the keypair alias are env-supplied — `SM_HOST`,
  `SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`,
  `SM_KEYPAIR_ALIAS` — supplied by the build box, never committed. So is
  `SOLSTONE_NESTED_SIGNING_TOKEN`.

Never commit the certificate, its credentials, key material, or `*.pfx` / `*.snk`
/ `*.p12` files. They are git-ignored; keep them out of the repo.
