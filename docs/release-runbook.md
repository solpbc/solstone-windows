# Release runbook

Releases are **operator-driven, by hand, from a known Windows build box.** There
is no GitHub Actions release path — `.github/workflows/` does not exist by policy.

## Verbs (never hand-chain the underlying tools)

| Step | Verb |
|---|---|
| Build binary + webview | `make build` |
| Deterministic composite gate (host checks · offline dependency policy · native Windows build/test) | `make ci` |
| Refresh RustSec data + check current advisories | `make audit` |
| Gate and pack a Velopack release into `Releases/` | `make package` |
| Pull the box's `Releases/` for a controlled aggregate workflow | `make pull-releases` |
| R2 direct-publication guard (**primary channel remains R2**) | `make publish-r2` (always fails closed) |
| GitHub direct-publication guard (optional, non-authoritative mirror) | `make publish` (always fails closed) |
| FlaUI smoke vs the installed app | `make smoke` |

## Packaging

- Velopack, per-user `%LocalAppData%`, **no UAC**.
- Evergreen WebView2 runtime (no fixed-version bundle).
- `Releases/` carries the full (+ delta) `nupkg`, `solstone-setup-{version}.exe`,
  `Solstone-win-Portable.zip`, and the feed (`releases.win.json`).
- Before any package work, the box checks the complete pinned contract in
  `packaging/release-toolchain.json`, derives the product version from Cargo
  metadata, and requires tracked `Cargo.lock` and `ui/package-lock.json` inputs.
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

## Update feed — R2 authoritative, optional GitHub mirror

The **primary auto-update feed is R2** at `updates.solstone.app/solstone-windows/`
— a privacy-clean static surface (no analytics, GET-only). The in-app updater
fetches `releases.win.json` from there with a bare, query-free manifest GET via
the custom local Velopack `UpdateSource`; package downloads still request the
package files by filename from the same first-party feed host. R2 is the
authoritative update feed. A GitHub Releases mirror is optional and
non-authoritative; its success cannot gate authoritative publication, update
delivery, or release evidence. Direct publication scripts are disabled; release
publication belongs to the aggregate provenance publisher. That future component
publishes each finalized signed release to R2 and may optionally mirror it to
GitHub. No GitHub mirror is required, and a missing or failed mirror never blocks
a release.

**Flow** (keeps publication credentials out of package construction):

1. Run `make audit`; a failed RustSec refresh produces no current advisory result
   and blocks the release.
2. On the build box: run `scripts/win-package.cmd` with `SOLSTONE_SIGN=1` in the
   environment for a signed release.
3. `make pull-releases` may pull the box's `Releases/` into a controlled aggregate
   workflow. It does not authorize or perform publication. The direct R2 target is
   a fail-closed guard.
4. Publication of finalized bytes and provenance to R2 and secondary channels
   belongs to the aggregate provenance publisher. It must upload immutable artifacts
   before mutable feed metadata. Any GitHub mirror is optional, non-authoritative,
   and never a release gate. It is a future component, not a runnable command
   documented here.

The aggregate publication layout accumulates version-named nupkgs, while the setup
installer is versioned per release, giving each release a never-reused URL. The
`solstone.app/download/windows` permalink points at the current release's versioned
installer.

## Package-manager channels (winget / scoop) — submission timing

These are secondary discovery surfaces; R2 remains authoritative, and any GitHub
mirror is optional and non-authoritative. They are **community-moderated**, so factor
the wait into release planning, don't block on it.

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
  genuinely urgent items moderators watch the community Discord.
- **scoop** — bucket PR, lighter process.
- **After aggregate publication, run `make check-channels`** — it derives the
  expected version from Cargo metadata, reads the live channels, and exits non-zero
  on drift. It does not repair drift; release publication belongs to the aggregate
  provenance publisher. winget once sat **ten releases stale** (0.2.0 while we
  shipped 0.2.10) before anyone noticed. Manifest inputs remain in-repo
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

**Signing environment.** The non-credential release-tool preflight first pins and
selects `smctl` and the exact x64 SignTool metadata without using either for signing
or verification. `scripts/package.ps1` passes the selected absolute `smctl` path to
`packaging/signing/preflight-auth.ps1`; those scripts then read signing configuration and
credentials from the environment, never from committed source: `SM_HOST`,
`SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`, and
`SM_KEYPAIR_ALIAS`. The operator supplies these on the build box at sign time;
they are never committed. The preflight fails fast (with a secret-free message) if
the environment is not provisioned or the credentials cannot sign.

`package.ps1` never invokes SignTool directly. Emitted-artifact signature and
content verification belongs to the later validator/finalizer, not this packaging
rail.

## Build-box gotchas

- Packaging consumes the exact cargo, npm, PowerShell, vpk, and smctl paths selected
  by `packaging/preflight-release-tools.ps1`; do not substitute ambient tools.
- Invoke `.cmd` shims via `cmd.exe /c`.
- The FlaUI smoke runs via a low-privilege scheduled task
  (`LogonType=Interactive`) into Session 1 against the installed app.
- Delta-update validation: install N → bump → package N+1 → after controlled
  aggregate publication to R2 →
  ready the update with `solstone-windows-app.exe --check-update` (asserts it
  finds N+1, downloads the *delta*, and stages it) → apply with
  `solstone-windows-app.exe --apply-update` (the CLI analogs of the in-app
  check / relaunch-to-install) → assert the relaunched app reports the new version
  via `--dump-state`. (The running app's auto-check timer is unit-tested; the CLI
  verbs make the delta mechanics deterministically verifiable headless.)

## Remote build host (optional)

`WIN_REMOTE_HOST=<host> make win-host-ci` takes a common-directory flock, refuses
untracked non-ignored files or an unmerged index, and snapshots the exact
committed, staged, and unstaged tracked working tree into a uniquely named,
verified git bundle carrying the CAS-guarded stable
`refs/heads/__swsync` ref. It ships the bundle by scp to
`swbuild.bundle` (no rsync); the box bootstrap hard-checks it out under
`~/swbuild` and runs `scripts/win-ci.cmd` for the build, tests, contract check,
and purity check. The caller accepts the run only when the box reports a
`WIN_CI_HEAD` matching the exact transferred snapshot SHA and includes
`WIN_CI_OK`; a stale or mismatched
acknowledgement fails even when compilation was green.
