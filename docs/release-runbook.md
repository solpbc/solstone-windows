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

## Signing (unsigned now → cert later)

The validated path is **unsigned**. `scripts/package.ps1` has an empty
`$SignTemplate` seam. When a code-signing cert is provisioned:

1. Populate `$SignTemplate` with the Velopack `--signTemplate` form.
2. Add `packaging/signing/preflight-auth.ps1` (credential pre-check).
3. Sign **release artifacts only**.

No code restructure is required to turn signing on.

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
