# packaging

Velopack release packaging for the observer.

- **Per-user, no UAC.** Installs to `%LocalAppData%`; never elevates.
- **Evergreen WebView2.** `vpk pack --framework webview2` makes `Setup.exe` install
  Microsoft's Evergreen WebView2 runtime on demand when it is absent (downloaded from
  MS's stable link, silent install) and no-op when already present; no runtime is
  bundled and there is no fixed-version bundle. Needs network at install time.
- **Update feed.** GitHub Releases serves a monotonic feed surface (full + delta
  `nupkg`, `Setup.exe`, feed JSON). Direct publication is locked; release
  publication belongs to the aggregate provenance publisher.

## Distribution

[`DISTRIBUTION.md`](DISTRIBUTION.md) — the channels (direct download, **winget**,
**scoop**), the aggregate publication boundary, and the update-ownership /
coexistence model. winget manifest reference: [`winget/`](winget/).

## Layout

- `hooks/` — the Velopack lifecycle handlers the app must be aware of
  (`--veloapp-install`, `--veloapp-update`, `--veloapp-obsolete`, `--veloapp-firstrun`).
  First-run registers the per-user autostart login item; the app being
  Velopack-aware is what makes the install hooks exit 0.
- `signing/` — release-artifact code signing (DigiCert KeyLocker / `smctl` via
  Velopack's `--signTemplate`). `scripts/package.ps1` signs when packaged with
  `-Sign` (release-only — `SOLSTONE_SIGN=1` on the box), and
  `signing/preflight-auth.ps1` is the pre-`vpk pack` credential check using the
  selected absolute smctl path. Credentials are env-supplied, never committed.
  Signing covers release artifacts only.

## Build

`make package` runs pinned release-tool preflight → metadata version gate →
tracked-lock guard → offline UI materialization/build → locked app build →
`scripts/package.ps1`/selected `vpk`. `Releases/` is a build output and is
git-ignored. The sole tool contract is `release-toolchain.json`.
