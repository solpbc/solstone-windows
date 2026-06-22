# Release runbook

Releases are **operator-driven, by hand, from a known Windows build box.** There
is no GitHub Actions release path — `.github/workflows/` does not exist by policy.

## Verbs (never hand-chain the underlying tools)

| Step | Verb |
|---|---|
| Build binary + webview | `make build` |
| Full gate (fmt · clippy · contract drift · tests · cargo-deny) | `make ci` |
| Pack a Velopack release into `Releases/` | `make package` |
| Upload `Releases/` to GitHub Releases (the update feed) | `make publish` |
| FlaUI smoke vs the installed app | `make smoke` |

## Packaging

- Velopack, per-user `%LocalAppData%`, **no UAC**.
- Evergreen WebView2 runtime (no fixed-version bundle).
- `Releases/` carries the full + delta `nupkg`, `Setup.exe`, and the feed JSON.
  GitHub Releases is the monotonic update feed.
- The app must be **Velopack-aware** so `--veloapp-*` hooks exit 0; first-run
  registers the per-user autostart login item.

## Signing (wired — opt-in, release-only)

Release artifacts are signed with the sol pbc code-signing certificate via
Velopack's `--signTemplate` (DigiCert KeyLocker / `smctl`). Signing is **opt-in
and release-only**: dev/local and delta-update-validation packs stay unsigned so
they do not burn the certificate's finite signature quota or churn the binary's
SmartScreen reputation hashes.

**Turn signing on for a release:** set `SOLSTONE_SIGN=1` in the build environment
before packaging — the box packaging wrapper forwards it as `-Sign` to
`scripts/package.ps1` (you can also pass `-Sign` directly). Without it the pack is
unsigned.

**Signing environment.** `scripts/package.ps1` and
`packaging/signing/preflight-auth.ps1` read the signing configuration and
credentials from the environment, never from committed source: `SM_HOST`,
`SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`, and
`SM_KEYPAIR_ALIAS`. The operator supplies these on the build box at sign time;
they are never committed. The preflight fails fast (with a secret-free message) if
the environment is not provisioned or the credentials cannot sign.

**Always `signtool verify /pa` after a signed pack** — on `Setup.exe` and the
packaged app exe. This is the authoritative gate: the signer can report success
even when a file was left unsigned, so a clean verify (sha256 + an RFC3161
timestamp, "Successfully verified") is what confirms the artifacts actually
shipped signed.

## Build-box gotchas

- Use explicit tool paths (`vpk` / `dotnet` / `gh`) until `PATH` refreshes after
  a package install.
- Invoke `.cmd` shims via `cmd.exe /c`.
- The FlaUI smoke runs via a low-privilege scheduled task
  (`LogonType=Interactive`) into Session 1 against the installed app.
- Delta-update validation: install N → bump → package N+1 → assert the *delta*
  applies and the relaunched app reports the new version via `--dump-state`.

## Remote build host (optional)

`WIN_REMOTE_HOST=<host> make win-host-ci` syncs the tree (dedicated remote tree,
`rsync --delete`) and runs `make ci` on the build box over SSH.
