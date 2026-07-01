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
entry (`DisplayName=solstone`, `Publisher=sol pbc`, `DisplayVersion`) that winget
uses for version detection.

## Release step

The package-manager channels are refreshed **as the last step of a release**, on
the release host, after the GitHub release + assets exist — they ride the same
artifacts, so there is no rebuild. The full sequence:

```
make package        # build box: build + Velopack pack -> Releases/
make publish        # build box: GitHub release (vX.Y.Z) + assets
make pull-releases   # release host: pull Releases/ from the box
make publish-r2      # release host: R2 update feed (in-app updater channel)
make publish-packages # release host: winget PR + scoop bump  <-- this file
```

`make publish-packages` runs `publish-winget` + `publish-scoop` (below). `VERSION`
defaults to the workspace version; override with `make publish-packages VERSION=x.y.z`.
There is no GitHub Actions / webhook path for this — it is an operator-run release
step, same posture as `publish` / `publish-r2`.

## winget

Manifests live in the community repo `microsoft/winget-pkgs` under
`manifests/s/solpbc/Solstone/<version>/` — **not** the Microsoft Store (no Store
account / MSIX). A reference copy of the current manifest set is in
[`winget/`](winget/).

**Per release: `make publish-winget`** (`scripts/publish-winget.sh`). winget has no
push API — every version is a PR to the community repo — so the target opens a
version-update PR via [`komac`](https://github.com/russellbanks/Komac) (which
fetches the `Setup.exe` asset, computes the SHA256, and submits). winget's pipeline
then validates (schema, hash, an interactive Windows-Sandbox install) before a
moderator/bot merges — so the PR is itself the install-validation gate.

- Needs `komac` on the release host (the script prints install instructions if absent).
- **First-ever package only** was a one-time `komac new` / hand PR; steady-state is `komac update`, which is what the target runs.
- **WebView2 dependency (carry-forward caveat).** The reference manifest declares
  `Dependencies.PackageDependencies: Microsoft.EdgeWebView2Runtime` (the Settings/About
  UI needs the Evergreen WebView2 runtime). `komac update` preserves the *last published*
  version's fields — so this block must exist in the merged winget manifest to propagate.
  The pending `0.2.0` new-package PR predates it, so the **first update PR after 0.2.0
  merges** must re-add the `Dependencies` block by hand (edit the komac-generated YAML
  before `--submit`, or use the manual fallback below). Once one published version carries
  it, subsequent `komac update`s keep it.
- Manual fallback: bump the YAML in [`winget/`](winget/) (`PackageVersion` /
  `InstallerUrl` / `InstallerSha256` / `AppsAndFeaturesEntries.DisplayVersion`) and
  open the PR by hand. Validate first with `winget validate --manifest <dir>`.

## scoop

Bucket: [`solpbc/scoop-solstone`](https://github.com/solpbc/scoop-solstone).
Users add it with `scoop bucket add solstone https://github.com/solpbc/scoop-solstone`.

The manifest (`bucket/solstone.json`) points at `Solstone-win-Portable.zip` and
carries `checkver`/`autoupdate` blocks (the latter let a maintainer auto-refresh
from a scoop checkout — `bin/checkver.ps1 solstone -u`).

**Per release: `make publish-scoop`** (`scripts/publish-scoop.sh`) — hashes the
published `Portable.zip` and bumps the manifest's `version` + `hash` in
`solpbc/scoop-solstone` via the GitHub API. No external CI/bot: it is an
operator-run release step. (Hand fallback: edit `bucket/solstone.json` directly.)

## Coexistence

Each install method has a single update owner (above) — never two. A user who
installs via winget is updated in place by the app; a user who installs via scoop
is updated by `scoop update`. Journal data lives in `%LocalAppData%\Solstone` and
is preserved across updates regardless of channel.
