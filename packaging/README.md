# packaging

Velopack release packaging for the observer.

- **Per-user, no UAC.** Installs to `%LocalAppData%`; never elevates.
- **Evergreen WebView2.** Relies on the OS WebView2 runtime; no fixed-version bundle.
- **Update feed.** GitHub Releases serves the monotonic feed (full + delta `nupkg`,
  `Setup.exe`, feed JSON). Operator-driven via `make publish` — there is no hosted CI release path.

## Distribution

[`DISTRIBUTION.md`](DISTRIBUTION.md) — the channels (direct download, **winget**,
**scoop**), the per-release manifest-update steps, and the update-ownership /
coexistence model. winget manifest reference: [`winget/`](winget/).

## Layout

- `hooks/` — the Velopack lifecycle handlers the app must be aware of
  (`--veloapp-install`, `--veloapp-update`, `--veloapp-obsolete`, `--veloapp-firstrun`).
  First-run registers the per-user autostart login item; the app being
  Velopack-aware is what makes the install hooks exit 0.
- `signing/` — release-artifact code signing (DigiCert KeyLocker / `smctl` via
  Velopack's `--signTemplate`). `scripts/package.ps1` signs when packaged with
  `-Sign` (release-only — `SOLSTONE_SIGN=1` on the box), and
  `signing/preflight-auth.ps1` is the pre-`vpk pack` credential check. Credentials
  are env-supplied, never committed. Signing covers release artifacts only.

## Build

`make package` runs `scripts/package.ps1` (build → `vpk pack` against this dir →
`Releases/`). `Releases/` is a build output and is git-ignored.
