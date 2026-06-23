# Release runbook

Releases are **operator-driven, by hand, from a known Windows build box.** There
is no GitHub Actions release path — `.github/workflows/` does not exist by policy.

## Verbs (never hand-chain the underlying tools)

| Step | Verb |
|---|---|
| Build binary + webview | `make build` |
| Full gate (fmt · clippy · contract drift · tests · cargo-deny) | `make ci` |
| Pack a Velopack release into `Releases/` | `make package` |
| Pull the box's `Releases/` to the release host | `make pull-releases` |
| Upload `Releases/` to the R2 update feed (**primary**) | `make publish-r2` |
| Upload `Releases/` to GitHub Releases (source mirror) | `make publish` |
| FlaUI smoke vs the installed app | `make smoke` |

## Packaging

- Velopack, per-user `%LocalAppData%`, **no UAC**.
- Evergreen WebView2 runtime (no fixed-version bundle).
- `Releases/` carries the full (+ delta) `nupkg`, `Solstone-win-Setup.exe`,
  `Solstone-win-Portable.zip`, and the feed (`releases.win.json`).
- The app must be **Velopack-aware** so `--veloapp-*` hooks exit 0; first-run
  registers the per-user autostart login item.

## Update feed — R2 primary, GitHub mirror

The **primary auto-update feed is R2** at `updates.solstone.app/solstone-windows/`
— a privacy-clean static surface (no analytics, GET-only). The in-app updater
fetches `releases.win.json` from there via Velopack's `HttpSource`. GitHub
Releases is a demoted **source-hygiene mirror** only.

**Two-host flow** (mirrors the macOS appcast split — keeps Cloudflare creds off
the signing box):

1. On the build box: `make package` (`-Sign` / `SOLSTONE_SIGN=1` for a release).
2. On the release host: `make pull-releases` (scp the box's `Releases/` over),
   then `make publish-r2` — uploads every artifact, **feed-last**
   (`releases.win.json` after the nupkgs/Setup.exe), then HEAD-checks the feed +
   the `Solstone-win-Setup.exe` permalink target. Requires `wrangler` authed to
   the Cloudflare account + `curl`.
3. Optional source mirror: on the box, `make publish` (tagged GitHub release).

`make publish-r2` accumulates: nupkgs are version-named (prior deltas/fulls stay),
`Solstone-win-Setup.exe` is a stable name overwritten with the latest. The
`solstone.app/download/windows` permalink 302s to that stable Setup.exe.

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
- Delta-update validation: install N → bump → package N+1 → publish to R2 →
  assert the running app auto-finds N+1, downloads the *delta*, and stages it;
  then apply headlessly with `solstone-windows-app.exe --apply-update` (the CLI
  analog of relaunch-to-install) and assert the relaunched app reports the new
  version via `--dump-state`.

## Remote build host (optional)

`WIN_REMOTE_HOST=<host> make win-host-ci` syncs the tree (dedicated remote tree,
`rsync --delete`) and runs `make ci` on the build box over SSH.
