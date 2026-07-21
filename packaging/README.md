# packaging

Velopack release packaging for the observer.

- **Per-user, no UAC.** Installs to `%LocalAppData%`; never elevates.
- **Evergreen WebView2.** `vpk pack --framework webview2` makes `Setup.exe` install
  Microsoft's Evergreen WebView2 runtime on demand when it is absent (downloaded from
  MS's stable link, silent install) and no-op when already present; no runtime is
  bundled and there is no fixed-version bundle. Needs network at install time.
- **Update feed.** R2 at `updates.solstone.app/solstone-windows/` is the
  authoritative update feed. A GitHub Releases mirror is optional and
  non-authoritative. Direct publication is locked; release publication belongs
  to the aggregate provenance publisher.

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
  Velopack's `--signTemplate`). `SOLSTONE_SIGN=1` selects the signed finalizer
  transaction; its resolver-selected authentication and signing actions use the
  selected absolute tool paths. Credentials are env-supplied, never committed.
  Signing covers release artifacts only.

## Build

`EXPECTED_RELEASE_COMMIT=<full-lowercase-commit>
SOLSTONE_ADVISORY_TREE_SHA256=<reviewed-lowercase-digest> make package` runs the
one source-bound build-to-finalize transaction. `Releases/` is an internal,
accumulated Velopack workspace and is distinct from the promoted
`target/release-candidate/<VERSION>/` current-only six/seven-artifact bundle plus
its companion manifest (seven/eight files total). Finalization receipts live
outside the candidate under `target/release-evidence/<VERSION>/`. All of these
paths are git-ignored build evidence. The sole tool contract is
`release-toolchain.json`, and every direct publication entry point remains
fail-closed because publication belongs to the aggregate provenance publisher.
