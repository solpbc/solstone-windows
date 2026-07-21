# Distribution channels

solstone for Windows has three distribution channels, all intended to point at the
**same finalized signed artifacts**. Direct publication is currently locked;
release publication belongs to the aggregate provenance publisher. R2 at
`updates.solstone.app/solstone-windows/` is the authoritative update feed; any
GitHub Releases mirror is optional and non-authoritative.

| Channel | Artifact | Who owns updates |
|---------|----------|------------------|
| Direct download (`solstone.app/download/windows`) | `solstone-setup-{version}.exe` | the app itself (in-app updater) |
| **winget** (`winget install solstone`) | `solstone-setup-{version}.exe` | the app itself; `winget upgrade` is a redundant catch-up |
| **scoop** (`scoop install solstone`) | `Solstone-win-Portable.zip` | scoop (the portable build's in-app updater is inert) |

The installer supports silent install (`Setup.exe --silent`), installs per-user to
`%LocalAppData%\Solstone` with no elevation, and registers an Add/Remove-Programs
entry that winget uses for version detection. **That entry's `DisplayName` is `sol`**
— Velopack names it after `--packTitle` (see `scripts/package.ps1`), *not* after the
pack id (`Solstone`). Manifest correlation fields must match it exactly or `winget
upgrade` cannot tell the package is installed.

## The manifests live in this repo

**`packaging/winget/` and `packaging/scoop/` are authoritative manifest inputs.**
Edit and review the copies here. Whole-manifest consumption belongs to the aggregate provenance publisher
rather than scripts that patch selected fields of live copies.

This is worth stating loudly because it was not true until 2026-07-13, and the
silence cost us. Both publish scripts used to be *version bumpers*: they took the
last **published** manifest and changed only version/url/hash. Every other field —
product copy, dependencies, ARP correlation, the scoop `bin` — carried forward from
whatever was first published, and nothing in this repo could correct it. Three live
defects came out of that single shape:

- **winget sat at 0.2.0 for ten releases.** `publish-winget.sh` shelled out to
  `komac update`, which requires `komac` on the release host. It was not installed,
  so the step exited 1 and was skipped in silence while every other channel shipped.
- **winget's published copy kept retired product vocabulary,** and its
  `AppsAndFeaturesEntries.DisplayName` said `Solstone` (not `sol`), so `winget
  upgrade` could not correlate the installed app. The corrected copy had been sitting
  in `packaging/winget/` the whole time — unread.
- **`scoop install solstone` was outright broken in 0.2.9 and 0.2.10.** The brand
  sweep renamed `--packTitle` to `sol`, so the portable zip's launcher became
  `sol.exe`; the bucket manifest went on shimming `Solstone.exe`, a file that no
  longer exists. `publish-scoop.sh` only ever touched version/url/hash, so it never
  noticed.

**The rule that prevents a fourth: aggregate publication must push the whole
manifest from this repo, never patch selected fields of the live one.**

## Release boundary

`EXPECTED_RELEASE_COMMIT=<full-lowercase-commit>
SOLSTONE_ADVISORY_TREE_SHA256=<reviewed-lowercase-digest> make package` runs the
complete source-bound build-to-finalize transaction. `Releases/` is its accumulated
Velopack workspace, not its distributable result. The promoted result is the
current-only `target/release-candidate/<VERSION>/` six/seven-artifact bundle plus
its companion manifest (seven/eight files total); the matching finalization
receipt is outside the candidate at
`target/release-evidence/<VERSION>/rust-release-finalization.json`. A successful
native proof adds `windows-native-proof.json` beside that receipt. `make
pull-releases` remains an artifact transport for a controlled aggregate workflow;
it does not publish or replace candidate validation.

`make publish`, `make publish-r2`, `make publish-winget`, `make publish-scoop`, and
their `publish-packages` aggregate are deliberate fail-closed guards. They accept no
version override and perform no authentication or transport. Release publication
belongs to the aggregate provenance publisher.

After aggregate publication, **`make check-channels`** remains read-only. It derives
the expected version from Cargo metadata, reports live winget/scoop state, and exits
non-zero on drift; it never repairs or publishes.

## winget

Manifests live in the community repo `microsoft/winget-pkgs` under
`manifests/s/solpbc/Solstone/<version>/` — **not** the Microsoft Store (no Store
account / MSIX). Our source of truth is [`winget/`](winget/).

The direct winget script is locked. Winget still requires a PR to the community
repository; the aggregate provenance publisher will own that submission after
the finalized candidate assets and provenance exist. Winget's pipeline validates schema,
hash, and an interactive Windows-Sandbox install before a moderator/bot merges.

- **Keep Actions disabled on the fork.** The fork inherits winget-pkgs' workflows, and
  every branch push triggers its Spell Checking run — which fails on package jargon and
  emails the fork owner a "build error" that looks like the PR failed. It is fork-local
  noise: upstream gates on CLA + the Azure validation pipeline only (verified against the
  merged first-package PR, which had no spell-check run). Disabled 2026-07-13
  (`gh api -X PUT repos/<owner>/winget-pkgs/actions/permissions -F enabled=false`);
  re-disable if the fork is ever recreated.
- Version-update PRs are the fast path — frequently auto-merged with no human,
  unlike the multi-day first-package gate.

**Two things a tool will try to "fix" wrongly — don't let it:**

- **`Architecture: x64` is correct.** Velopack's `Setup.exe` is a 32-bit stub, so
  anything that sniffs the PE header (komac did) writes `x86`. The application it
  installs, `solstone-windows-app.exe`, is PE32+ x86-64. Architecture describes the
  app's applicability, not the stub.
- **`AppsAndFeaturesEntries.DisplayName: sol`** — the ARP name, not `Solstone`.
  Every field listed there must match the real ARP entry or correlation fails.

## scoop

Bucket: [`solpbc/scoop-solstone`](https://github.com/solpbc/scoop-solstone).
Users add it with `scoop bucket add solstone https://github.com/solpbc/scoop-solstone`.

Our source of truth is [`scoop/solstone.json`](scoop/solstone.json). It points at
`Solstone-win-Portable.zip` and carries `checkver`/`autoupdate` blocks (the latter
let a maintainer auto-refresh from a scoop checkout — `bin/checkver.ps1 solstone -u`).

The direct scoop script is locked. Hashing the finalized `Portable.zip` and
publishing the complete reviewed manifest to `solpbc/scoop-solstone` belongs to the aggregate provenance publisher.

**`bin` / `shortcuts` must name a file that exists in the portable zip.** Velopack
names the top-level launcher after `--packTitle` — today `sol.exe`. If `--packTitle`
ever changes again, these change with it, or `scoop install` breaks at shim time.
Check with `unzip -Z1 Solstone-win-Portable.zip`.

## Coexistence

Each install method has a single update owner (above) — never two. A user who
installs via winget is updated in place by the app; a user who installs via scoop
is updated by `scoop update`. Journal data lives in `%LocalAppData%\Solstone` and
is preserved across updates regardless of channel.
