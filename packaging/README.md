# packaging

Velopack release packaging for the observer.

- **Per-user, no UAC.** Installs to `%LocalAppData%`; never elevates.
- **Evergreen WebView2.** Relies on the OS WebView2 runtime; no fixed-version bundle.
- **Update feed.** GitHub Releases serves the monotonic feed (full + delta `nupkg`,
  `Setup.exe`, feed JSON). Operator-driven via `make publish` — there is no hosted CI release path.

## Layout

- `hooks/` — the Velopack lifecycle handlers the app must be aware of
  (`--veloapp-install`, `--veloapp-update`, `--veloapp-obsolete`, `--veloapp-firstrun`).
  First-run registers the per-user autostart login item; the app being
  Velopack-aware is what makes the install hooks exit 0.
- `signing/` — the release-artifact signing seam. **Empty now** (the validated
  path is unsigned). When the cert is provisioned, `scripts/package.ps1` populates
  its `$SignTemplate` with the Velopack `--signTemplate` form and `signing/`
  gains a credential pre-check. Signing covers release artifacts only.

## Build

`make package` runs `scripts/package.ps1` (build → `vpk pack` against this dir →
`Releases/`). `Releases/` is a build output and is git-ignored.
