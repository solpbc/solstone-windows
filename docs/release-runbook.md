# Release runbook

Releases are **operator-driven, by hand, from a known Windows build box.** There
is no GitHub Actions release path — `.github/workflows/` does not exist by policy.

## Verbs (never hand-chain the underlying tools)

| Step | Verb |
|---|---|
| Build binary + webview | `make build` |
| Deterministic composite gate (host checks · offline dependency policy · native Windows build/test) | `make ci` |
| Refresh RustSec data + check current advisories | `make audit` |
| Pack a Velopack release into `Releases/` | `make package` |
| Pull the box's `Releases/` to the release host | `make pull-releases` |
| Upload `Releases/` to the R2 update feed (**primary**) | `make publish-r2` |
| Upload `Releases/` to GitHub Releases (**required** mirror) | `make publish` |
| FlaUI smoke vs the installed app | `make smoke` |

## Packaging

- Velopack, per-user `%LocalAppData%`, **no UAC**.
- Evergreen WebView2 runtime (no fixed-version bundle).
- `Releases/` carries the full (+ delta) `nupkg`, `solstone-setup-{version}.exe`,
  `Solstone-win-Portable.zip`, and the feed (`releases.win.json`).
- The app must be **Velopack-aware** so `--veloapp-*` hooks exit 0; first-run
  registers the per-user autostart login item.

## Release notes — cut the CHANGELOG section before a signed pack

Per-release notes ship **inside the update feed**: `make package` extracts the
`CHANGELOG.md` `## [<version>]` section and threads it into `vpk pack` via
`--releaseNotes`, so `releases.win.json` carries `NotesMarkdown`/`NotesHtml`. The
in-app Updates pane and `solstone.app/releases/windows` render those notes — the
Windows analog of the macOS appcast `<description>`.

**Before a signed release pack, cut the CHANGELOG:** rename `## [Unreleased]` to
`## [<version>] - <YYYY-MM-DD>` (Keep a Changelog format) so a matching section
exists. A signed pack (`-Sign` / `SOLSTONE_SIGN=1`) **fails loud** if the section
is missing — same discipline the macOS `publish-appcast.py` enforces. Unsigned
dev/local packs warn and pack note-less, so iteration stays frictionless.

## Update feed — R2 primary, GitHub mirror

The **primary auto-update feed is R2** at `updates.solstone.app/solstone-windows/`
— a privacy-clean static surface (no analytics, GET-only). The in-app updater
fetches `releases.win.json` from there with a bare, query-free manifest GET via
the custom local Velopack `UpdateSource`; package downloads still request the
package files by filename from the same first-party feed host. GitHub Releases is
a **required source-hygiene mirror** — every signed release publishes to **both**
R2 (primary) and GitHub (mirror), and **both carry the same per-release notes**
(the `CHANGELOG.md ## [<version>]` section). The GitHub mirror is the
download/source-of-record surface winget/scoop reference and where the tagged
release + signed artifacts live; it is never skipped on a real release.

**Flow** (mirrors the macOS appcast split — keeps Cloudflare creds off the
signing box; both publishes are mandatory):

1. Run `make audit`; a failed RustSec refresh produces no current advisory result
   and blocks the release.
2. On the build box: run `scripts/win-package.cmd` with `SOLSTONE_SIGN=1` in the
   environment for a signed release.
3. On the release host: `make pull-releases` (scp the box's `Releases/` over),
   then `make publish-r2` — uploads every artifact, **feed-last**
   (`releases.win.json` after the nupkgs/Setup.exe), then HEAD-checks the feed +
   the `solstone-setup-{version}.exe` artifact. Requires `wrangler` authed to
   the Cloudflare account + `curl`.
4. **Required GitHub mirror:** on the **release host** (same host as `publish-r2`,
   not the build box — the box has no `gh`), `make publish` → `scripts/publish-gh.sh`
   creates the tagged `v<version>` GitHub release, attaches every `Releases/`
   artifact (feed JSON last), and sets the release body from the
   `CHANGELOG.md ## [<version>]` section via `gh --notes-file` (same notes as the
   R2 feed; bare-title fallback only if the section is absent). Version is the
   highest packed full nupkg (`sort -V`), so an accumulated `Releases/` still tags
   the current release. Fails loud if the tag already exists (no silent overwrite).
   Requires `gh` authed to the repo.

`make publish-r2` accumulates: nupkgs are version-named (prior deltas/fulls stay),
and the setup installer is versioned per release, giving each release a
never-reused URL. The `solstone.app/download/windows` permalink points at the
current release's versioned installer.

## Package-manager channels (winget / scoop) — submission timing

These are secondary discovery surfaces (R2 + GitHub are the source-of-record); they
are **community-moderated**, so factor the wait into release planning, don't block on it.

- **winget (`microsoft/winget-pkgs`).** A **first/new-package** PR for a publisher is
  the gated, slow step: after the Azure validation pipeline (~30-40 min) labels it
  `Azure-Pipeline-Passed`/`Validation-Completed`, it sits on a **human (volunteer)
  moderator** approval (`REVIEW_REQUIRED` → `Moderator-Approved` → auto-merge). Empirical
  (gh, June 2026): new-package merges run a **median ~3.7 days, p90 ~6 days, tail to
  1-2 weeks** (weekends slow it). **Subsequent version-update PRs are the fast path** —
  median **~2 hours**, frequently auto-merged with no human (a "verified developer"
  self-serve path is in development). So: land the first package once, then version bumps
  are near-instant (build a little slack for the occasional one that hits the manual
  queue). Don't close/reopen or push empty commits to "nudge" (resets validation); for
  genuinely urgent items moderators watch the community Discord. (Research:
  `records/decisions/260625-vpx-solstone-windows-settings-native-redesign-0.2.4.md`.)
- **scoop** — bucket PR, lighter process.
- **After publishing, run `make check-channels`** — it asserts each channel actually
  carries the release and exits non-zero on drift. `make publish-packages` is an
  operator step nothing forces, and a channel that is never updated raises no error;
  it just keeps serving the old version. winget sat **ten releases stale** (0.2.0
  while we shipped 0.2.10) before anyone noticed. Manifest source of truth is in-repo
  (`packaging/winget/`, `packaging/scoop/`) — see `packaging/DISTRIBUTION.md`.
- **Chocolatey** — a third channel (enterprise/IT-admin reach) we have **not** adopted;
  its community repo is also human-moderated. Evaluate deliberately, below winget/scoop.

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
  ready the update with `solstone-windows-app.exe --check-update` (asserts it
  finds N+1, downloads the *delta*, and stages it) → apply with
  `solstone-windows-app.exe --apply-update` (the CLI analogs of the in-app
  check / relaunch-to-install) → assert the relaunched app reports the new version
  via `--dump-state`. (The running app's auto-check timer is unit-tested; the CLI
  verbs make the delta mechanics deterministically verifiable headless.)

## Remote build host (optional)

`WIN_REMOTE_HOST=<host> make win-host-ci` syncs the tree (dedicated remote tree,
`rsync --delete`) and runs `make ci` on the build box over SSH.
