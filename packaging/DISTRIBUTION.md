# Distribution channels

solstone for Windows ships through three channels, all pointing at the **same
OV-signed artifacts** published per release to the GitHub release (`vX.Y.Z`) and
the R2 update feed:

| Channel | Artifact | Who owns updates |
|---------|----------|------------------|
| Direct download (`solstone.app/download/windows`) | `Solstone-win-Setup.exe` | the app itself (in-app updater) |
| **winget** (`winget install solstone`) | `Solstone-win-Setup.exe` | the app itself; `winget upgrade` is a redundant catch-up |
| **scoop** (`scoop install solstone`) | `Solstone-win-Portable.zip` | scoop (the portable build's in-app updater is inert) |

The installer supports silent install (`Setup.exe --silent`), installs per-user to
`%LocalAppData%\Solstone` with no elevation, and registers an Add/Remove-Programs
entry (`DisplayName=Solstone`, `Publisher=sol pbc`, `DisplayVersion`) that winget
uses for version detection.

## winget

Manifests live in the community repo `microsoft/winget-pkgs` under
`manifests/s/solpbc/Solstone/<version>/` — **not** the Microsoft Store (no Store
account / MSIX). A reference copy of the current manifest set is in
[`winget/`](winget/).

**Per release:**

1. Cut + publish the release (so the `Solstone-win-Setup.exe` asset exists at
   `https://github.com/solpbc/solstone-windows/releases/download/v<version>/`).
2. Update the manifest and open a PR. The easiest path is
   [`komac`](https://github.com/russellbanks/Komac) or
   [`wingetcreate`](https://github.com/microsoft/winget-create):

   ```sh
   komac update solpbc.Solstone \
     --version <version> \
     --urls https://github.com/solpbc/solstone-windows/releases/download/v<version>/Solstone-win-Setup.exe \
     --submit
   ```

   (komac fetches the installer, computes the SHA256, and opens the PR.) Or edit
   the YAML in `winget/`, bump `PackageVersion` + `InstallerUrl` + `InstallerSha256`
   + `AppsAndFeaturesEntries.DisplayVersion`, and submit the PR by hand.
3. winget's automated pipeline validates (schema, hash, an interactive
   Windows-Sandbox install) before a moderator merges.

Validate locally before submitting: `winget validate --manifest <dir>` (and, on a
real interactive desktop, `winget install --manifest <dir>`).

## scoop

Bucket: [`solpbc/scoop-solstone`](https://github.com/solpbc/scoop-solstone).
Users add it with `scoop bucket add solstone https://github.com/solpbc/scoop-solstone`.

The manifest (`bucket/solstone.json`) points at `Solstone-win-Portable.zip` and
carries `checkver`/`autoupdate` blocks.

**Per release**, refresh the manifest's `version` + `hash` either by:

- running scoop's updater locally against the bucket
  (`bin/checkver.ps1 solstone -u` from a scoop checkout), or
- editing `bucket/solstone.json` by hand (bump `version`, and `hash` to the
  SHA256 of the new `Solstone-win-Portable.zip`), or
- triggering the repo's manual **Excavator** workflow if/when it is added (it is
  intentionally left as a manual, operator-run step).

## Coexistence

Each install method has a single update owner (above) — never two. A user who
installs via winget is updated in place by the app; a user who installs via scoop
is updated by `scoop update`. Journal data lives in `%LocalAppData%\Solstone` and
is preserved across updates regardless of channel.
