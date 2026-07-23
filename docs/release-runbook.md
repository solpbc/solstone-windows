# Release runbook

Releases are **operator-driven, by hand, from a known Windows build box.** There
is no GitHub Actions release path — `.github/workflows/` does not exist by policy.

## Verbs (never hand-chain the underlying tools)

| Step | Verb |
|---|---|
| Build binary + webview | `make build` |
| Deterministic composite gate (host checks · offline dependency policy · native Windows build/test) | `make ci` |
| Refresh RustSec data + check current advisories | `make audit` |
| Prove the materialized release advisory config with cargo-deny 0.20.2 offline | `make check-release-advisory-config` |
| Verify Rust release-manifest evidence offline | `make check-rust-release-manifest` |
| Source-bound build and atomic finalization | Set the release commit, advisory digest, and the three mirror-packet variables below, then run `make package` |
| Prove one exact signed candidate by isolated install and explicit smoke | `make prove-rust-release-native RELEASE_DIR=target/release-candidate/<VERSION>` |
| Publish retained release evidence after delivery | `make publish-transparency RELEASE_DIR=target/release-candidate/<VERSION>` |
| Pull the box's `Releases/` for a controlled aggregate workflow | `make pull-releases` |
| R2 direct-publication guard (**primary channel remains R2**) | `make publish-r2` (always fails closed) |
| GitHub direct-publication guard (optional, non-authoritative mirror) | `make publish` (always fails closed) |
| FlaUI smoke vs the installed app | `make smoke` |

## Packaging

- Velopack, per-user `%LocalAppData%`, **no UAC**.
- Evergreen WebView2 runtime (no fixed-version bundle).
- `Releases/` is the accumulated internal Velopack workspace. It is not a
  finalized candidate and must not be used as publication evidence.
- The finalizer promotes one current-only six/seven-artifact bundle plus its
  companion manifest (seven/eight files total) at
  `target/release-candidate/<VERSION>/`.
- Before cleanup or build, the transaction checks the closed resolver selection,
  Cargo metadata version, full lowercase `EXPECTED_RELEASE_COMMIT`, local
  lineage, allowed `main`/`__swsync` ref, clean source state, and SHA-256 of both
  `Cargo.lock` and `ui/package-lock.json`.
- The app must be **Velopack-aware** so `--veloapp-*` hooks exit 0; first-run
  registers the per-user autostart login item.

## Source-bound finalization

Set `EXPECTED_RELEASE_COMMIT` to the exact full lowercase release commit.
Provision a clean cargo home containing only the approved private RustSec mirror
cache, set `SOLSTONE_ADVISORY_MIRROR_LOCATOR` to that mirror's private Git URL,
and run `make check-release-advisory-config`. This maps the cache to the isolated
`target/release-advisory-db/` shape and proves that cargo-deny 0.20.2 accepts the
locator-bound generated config offline. This config-only verb reads the locator
only; it does not consume or verify the freshness packet.

For finalization, obtain the operator-supplied mirror freshness receipt body and
its adjacent `<body>.minisig`, plus the approved mirror public-key file. Keep all
three outside `target/release-advisory-db/`, put `minisign` on `PATH`, and set:

- `SOLSTONE_ADVISORY_MIRROR_LOCATOR` to the private mirror Git URL.
- `SOLSTONE_ADVISORY_RECEIPT` to the absolute receipt-body path.
- `SOLSTONE_ADVISORY_MIRROR_PUB` to the absolute public-key path.

The finalizer first reads the packet files once, checks the supplied public-key
file bytes against the committed SHA-256 pin, verifies the body and trusted
comment with `minisign -V`, enforces the signed UTC freshness window, and obtains
the signed mirror commit. It then renders locator-bound cargo-deny config,
inspects the clean full mirror repository and exact origin offline, requires
repository `HEAD` to equal the signed commit, and finally runs cargo-deny
offline. `advisory_checked_at` is earned only after that final check succeeds.
Any failure occurs before application build. Never commit a production mirror
packet, signature, or operator public-key file; supply them from the controlled
release environment.

The isolated database root may also contain cargo-deny's regular top-level
`db.lock`; finalization tolerates but never removes that file. A link, special
file, or any other extra child still fails snapshot classification.

Review the isolated RustSec repository at the intended full commit before
finalization. From that reviewed repository, compute the archive-tree digest
with `git -C <isolated-advisory-db-repo> archive --format=tar HEAD | sha256sum`
and set its 64-lowercase-hex result as `SOLSTONE_ADVISORY_TREE_SHA256`. This is an
independent operator input: do not auto-derive it inside finalization from the
same database being checked, because that would make swapped-database detection
circular.

After the inline commit and advisory-digest checks, the package bootstrap asks
the selected npm to run `--prefix ui ci --offline --dry-run`. This probe does not
materialize `ui/node_modules`; if the cache is incomplete, run `make install` on
the build box with network access and restart the source-bound package command.
The dry run's cache-honesty must also be confirmed on the box with warm and
deliberately incomplete caches during post-ship verification.

With the three mirror-packet variables above set,
`EXPECTED_RELEASE_COMMIT=<commit> SOLSTONE_ADVISORY_TREE_SHA256=<digest> make package`
delegates through
`scripts/package.ps1` to the single xtask finalizer. That transaction owns npm
materialization/build, the locked release build, Velopack packing, optional
KeyLocker signing, selected SignTool verification, executable cross-container
identity, manifest rendering, strict classification, the final source/lock
recheck, and atomic promotion. Direct `scripts/package.ps1` and
`scripts/win-package.cmd` reach the same transaction; neither attests a
pre-existing executable.

The manifest executable authority is equality between the canonical full-nupkg
and portable members. The pre-pack executable hash is diagnostic only on a
container divergence: signed vpk operates on private copies, so signed container
bytes legitimately differ from the unsigned stage. The stage remains
transaction-bound structurally: `create_transaction_paths` creates and verifies
it new and empty after cleanup, the build uses a transaction-local
`CARGO_TARGET_DIR`, exactly one executable is copied into the stage, and vpk
packs only that directory. A missing pre-pack diagnostic does not weaken or fail
the two-container equality gate.

The finalizer assembles the candidate in a newly empty sibling temporary and
atomically renames the whole directory to
`target/release-candidate/<VERSION>/`. It never writes candidate members
piecemeal into the final path. The six canonical artifact files are
`assets.win.json`, `RELEASES`, `releases.win.json`, the current full nupkg, the
versioned setup executable, and the portable zip. A current delta is the seventh
artifact only when all current ledgers advertise it. The companion manifest is
written last, making seven or eight files total.

After candidate promotion, the finalizer promotes
`target/release-evidence/<VERSION>/rust-release-finalization.json`. This receipt
is outside the candidate and cross-binds its manifest filename and SHA-256,
source commit, both lock digests, candidate path/count, selection-record hash,
signing mode, and isolated RustSec source/commit/archive digest/acquisition and
check times. Receipt bytes are never candidate members.

## Offline Rust release-manifest verification

`make check-rust-release-manifest` has three explicit modes:

- With neither selector set, it verifies the exact embedded schema, committed
  semantic/classifier fixtures, ledger grammar, and deterministic rendering.
- `MANIFEST=<path> make check-rust-release-manifest` validates one manifest
  against the current checkout and verifies every exact named sibling byte. A
  success explicitly does not classify the directory as complete or publishable.
- `RELEASE_DIR=<path> make check-rust-release-manifest` validates a flat,
  current-only complete bundle. `MANIFEST` and `RELEASE_DIR` cannot be set
  together.

The complete directory contains the companion
`solstone-windows-x86_64-pc-windows-msvc.rust-release-manifest.json` plus exactly
the following manifest-listed files:

1. `assets.win.json`
2. `RELEASES`
3. `releases.win.json`
4. `Solstone-<VERSION>-full.nupkg`
5. `solstone-setup-<VERSION>.exe`
6. `Solstone-win-Portable.zip`
7. `Solstone-<VERSION>-delta.nupkg`, only when the current feeds advertise it

The companion is not self-listed, so the complete directory has seven files,
or eight with a current delta. Historical records may remain inside the two
cumulative ledgers, but historical package bytes, subdirectories, and unknown
files are rejected. `RELEASES` remains its BOM-prefixed `SHA1 filename size`
full-package ledger; `releases.win.json` uses the raw `NotesHTML` key. This gate
does not construct, sign, authenticate, or publish a release. Direct publication
commands remain fail-closed.

## Release notes — cut the CHANGELOG section before finalization

Per-release notes ship **inside the update feed**: `make package` extracts the
`CHANGELOG.md` `## [<version>]` section and threads it into `vpk pack` via
`--releaseNotes`, so `releases.win.json` carries `NotesMarkdown`/`NotesHTML`. The
in-app Updates pane and `solstone.app/releases/windows` render those notes — the
Windows analog of the macOS appcast `<description>`.

**Before a signed release pack, cut the CHANGELOG:** rename `## [Unreleased]` to
`## [<version>] - <YYYY-MM-DD>` (Keep a Changelog format) so a matching section
exists. Every finalization, signed or unsigned, fails closed when the section is
missing; cut and review it before starting the transaction.

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

1. Provision the approved mirror cache, set
   `SOLSTONE_ADVISORY_MIRROR_LOCATOR`, and run
   `make check-release-advisory-config`; a failed materialization or offline
   cargo-deny check ends this step before finalization begins.
2. On the clean build box, supply the current body plus adjacent `.minisig`, the
   approved public-key file, and `minisign` on `PATH`. Set the three mirror-packet
   variables, the full lowercase `EXPECTED_RELEASE_COMMIT`, and the independently
   reviewed `SOLSTONE_ADVISORY_TREE_SHA256`; then run `SOLSTONE_SIGN=1 make
   package` (or the thin `.cmd` wrapper) for a signed candidate.
3. Run `make prove-rust-release-native
   RELEASE_DIR=target/release-candidate/<VERSION>`. A green proof atomically adds
   `target/release-evidence/<VERSION>/windows-native-proof.json` outside the
   candidate.
4. `make pull-releases` may pull the box's accumulated `Releases/` into a
   controlled aggregate workflow. It does not authorize publication and is not a
   substitute for the finalized candidate and receipts. The direct R2 target is a
   fail-closed guard.
5. Publication of finalized bytes and provenance to R2 and secondary channels
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
before finalization. The thin wrappers select the signed resolver record and the
xtask transaction; without it the candidate is unsigned and is categorically
ineligible for native proof.

**Signing environment.** The non-credential release-tool preflight pins and
selects `smctl`, the exact x64 SignTool, and their closed action templates. The
finalizer constructs the signing-mode child environment from the pinned
selection record and gives the same one-key overlay to authentication preflight
and Velopack: `PATH` is the selected SignTool directory prepended with a literal
`;` to the record's full activated `PATH`. It does not read ambient `PATH`, add
compiler variables, or retry with another environment. MSVC/vcvars activation
is not what supplies SignTool. Other environment values remain inherited, so
the selected authentication/signing actions read signing configuration and
credentials from the operator's environment, never from committed source:
`SM_HOST`, `SM_API_KEY`, `SM_CLIENT_CERT_FILE`, `SM_CLIENT_CERT_PASSWORD`, and
`SM_KEYPAIR_ALIAS`. The operator supplies these on the build box at sign time;
they are never committed. The preflight uses the real signing-child environment
and fails fast (with a secret-free message) if it is not provisioned or the
credentials cannot sign.

After Velopack emits the final setup bytes, the finalizer invokes the actual
resolver-selected SignTool with `/pa /all /v`. It requires one Authenticode
signature, the public policy leaf thumbprint, trusted chain-policy success,
one timestamp statement, and one verified timestamp certificate chain
terminating at the successful-verification line. That output states no
timestamp protocol, so RFC 3161 is established at sign time by the KeyLocker
`--signTemplate` path, not asserted from verify output. Parsed success earns
manifest `native_tools.signing_mode = "signed-verified"`; process exit or
success prose alone is insufficient.

## Native proof

`make prove-rust-release-native RELEASE_DIR=<candidate>` first acquires read-only
checkout facts, then runs strict whole-directory classification and hashes the
companion manifest as candidate identity. Only then does it resolve native
action tools for signed preflight, install, and smoke. It accepts only
`signed-verified` candidates with a matching finalization receipt and matching
nupkg/portable canonical executable members.

The atomic Make wrapper builds xtask with the selected Cargo and then invokes the
built binary directly, honoring `CARGO_TARGET_DIR` and the platform executable
suffix. Direct invocation avoids adding the rustup toolchain bin directory to the
signed preflight's `PATH`. The wrapper also binds xtask's version-gate Cargo to
the same selected executable, preserving the Cargo identity used for checkout
metadata acquisition.

Native proof obtains Git from `GIT`, defaulting to the single-component name
`git`, which the operating system resolves through its search path. It obtains
PowerShell from `SOLSTONE_PROOF_POWERSHELL`, defaulting to `powershell`, and
`resolve_bootstrap` resolves that single-component name through `PATH` to the
absolute program required by `ProcessCommandRunner`; either variable may instead
name an absolute executable. This differs deliberately from finalization: the
source-bound signing transaction accepts only the absolute `GIT` established by
its package bootstrap. Name search or an absolute override are the only supported
bootstrap modes; there is no third resolution mechanism.

The command installs only the candidate's canonical versioned setup into a newly
empty proof-owned `LOCALAPPDATA` root outside `RELEASE_DIR`. The canonical app
must be absent before setup and created afterward; no existing-install no-op or
setup fallback is accepted. The installed app, both containers, and manifest
baseline must agree by SHA-256 and byte count. The explicit installed binary must
report the candidate version through STEP_8's direct `--dump-state` invocation.
The selected smoke must run with fallback disabled, emit the load-bearing
`SMOKE_OK` health/render evidence, and verify the launched Session-1 instance's
version from `/healthz`. Post-smoke strict validation and companion bytes must be
unchanged.

Success atomically writes
`target/release-evidence/<VERSION>/windows-native-proof.json`. The receipt records
only normalized identity, hashes, explicit install/smoke success, isolated-clean
mode, and UTC proof time; it carries no host, account, credential, certificate, or
absolute install path. This host-tested orchestration uses fakes; real install,
certificate, and Session-1 evidence is earned only by running it on the box.

## Release transparency

Run `make publish-transparency RELEASE_DIR=target/release-candidate/<VERSION>`
only after the aggregate provenance publisher has completed authoritative
delivery; transparency publication never gates or rolls back that delivery.
Both publishing and retrying require the local checkout to sit on the
candidate's exact commit with a clean working tree; xtask inherits this from
the release validator's live-checkout facts and refuses a dirty or mismatched
checkout. It snapshot-copies and re-validates the retained candidate, archives
the complete evidence and artifact bytes first, then publishes only the
manifest, native proof when required, signed ledger entry, derived ledger, and
signed latest pointer. Artifact bytes never reach the public transparency
surface.

The command is environment-driven. `TRANSPARENCY_S3_ENDPOINT`,
`TRANSPARENCY_BUCKET`, `TRANSPARENCY_S3_ACCESS_KEY_ID`,
`TRANSPARENCY_S3_SECRET_ACCESS_KEY`, `TRANSPARENCY_MINISIGN_KEY`,
`TRANSPARENCY_MINISIGN_PUB`, and `TRANSPARENCY_ARCHIVE_CHANNEL` are required.
`TRANSPARENCY_BASE_URL` defaults to `https://transparency.solstone.app`, and
`TRANSPARENCY_GENESIS=1` is explicit one-time approval for a verified empty
product prefix. The public trust-anchor filename
`solpbc-transparency-1.pub` and served location
`releases/keys/solpbc-transparency-1.pub` are public contract. Rotation
increments the numeric suffix and uses cross-signed successor files.
`TRANSPARENCY_MINISIGN_PUB` supplies only the operator's local path to that key;
it does not change the public filename or served location. The publisher does
not upload the key; the operator provisions the served location. Do not give a
local key a `.key` suffix because the repository ignore policy silently hides
that suffix. No production public key is committed to this repository.

xtask asks once for the secret-key passphrase with a no-echo terminal reader,
keeps the bytes only in memory for that command invocation, and feeds the exact
newline-terminated bytes to each required pinned-minisign signing process on
stdin. If passphrase acquisition fails, xtask falls back to minisign's existing
interactive no-echo prompt for each required signature. A failed stdin-fed
signature is terminal for that attempt and does not trigger the fallback.

Successful stdin feeding reduces a fresh publish from two operator prompts to
one; the adoption, staged-retry, and resign counts remain 0, 0, and 1.

| Operator path | Successful reader | Acquisition-failure fallback |
|---|---:|---:|
| Fresh publish | 1 prompt | 2 minisign prompts |
| Create-only PUT adoption | 0 prompts, then 1 on the next invocation | 0 prompts, then 1 minisign prompt on the next invocation |
| Byte-staged retry | 0 prompts | 0 prompts |
| `make resign-transparency-pointer` | 1 prompt | 1 minisign prompt |

The passphrase never enters argv, the child environment, a file, or an operator
diagnostic.

Publication is idempotent and retryable, including when immutable evidence was
created but the mutable pointer was not committed. A staged retry reuses the
same entry and signature bytes and archives them before resuming public writes.
Each version key is one-shot and permanent. If a create-only race records a
different but valid own attempt, the next invocation first archives the adopted
bytes and then resumes.

Only a successful `ARCHIVED <digest>` channel result creates the durable local
acknowledgment
`.release-transparency-recovery/solstone-windows/<VERSION>/archive-ack.v1`.
The pointer recovery pair lives below the same operator-local directory, and
both records survive `make clean`. The ephemeral `stage-manifest.v1` below
`target/release-transparency-stage/` is a byte-reuse record, not an archive
acknowledgment.

The pointer signature is written before its body, and the body is the commit
boundary. Consumers retry on a pointer/signature mismatch because that
recognized transient can occur between those two writes. Mutable writes are
unreachable after a failed immutable-byte verification.

A staged retry remains valid even when its pointer has aged beyond fourteen
days. Complete that retry with the persisted bytes, then run
`make resign-transparency-pointer` to refresh only `signed_at`, `valid_until`,
and the pointer signature without changing the chain length or tip digest.

If the remote version prefix is empty and no `archive-ack.v1` acknowledgment
exists, a local attempt never reached publication and the operator may discard
only that version's directories at
`target/release-transparency-stage/solstone-windows/<VERSION>/` and
`.release-transparency-recovery/solstone-windows/<VERSION>/`. If either a remote
version object or an `archive-ack.v1` acknowledgment exists, the version is
permanent; cut the next version instead of deleting, replacing, or weakening
retention.

The surface attests what was released, that it is immutable, and that history
is publicly reconstructible. It does not claim that binaries provably match
source.

The tracked [transparency head log](../transparency-head-log.jsonl) is a second
head witness carried by ordinary repository commits. GitHub is optional and
non-authoritative, is never required, and cannot gate release or transparency
publication.

## Build-box gotchas

- In signed mode the finalizer, not MSVC/vcvars activation, supplies SignTool to
  authentication preflight and Velopack from the pinned selection record. Do not
  repair or override that child `PATH` with the shell's ambient `PATH`.
- The release contract pins Windows PowerShell 5.1. Invoke the make-backed release
  rail as `PWSH=powershell make preflight-release-tools` / `PWSH=powershell make
  package`, or use the box-native `scripts/win-package.cmd`.
- Packaging consumes the exact cargo, npm, PowerShell, vpk, and smctl paths selected
  by `packaging/preflight-release-tools.ps1`; do not substitute ambient tools.
- `packaging/preflight-release-tools.ps1` must emit its one selection record as
  UTF-8 on stdout over every transport; the native-proof resolver decodes it
  strictly and rejects legacy-codepage bytes.
- Canonical checkout paths keep their Windows verbatim form for containment and
  identity checks, but child-process path text permits only drive and UNC forms
  and removes their verbatim prefix. The ordinary child paths therefore inherit
  the 260-character limit of tools that do not opt in to long paths. For the
  current layout and version, the longest repository-constructed finalizer path
  is 78 characters beyond the checkout and the longest native-proof path is 90.
  Those counts do not bound or prove Cargo- or Tauri-created descendants beneath
  `CARGO_TARGET_DIR`, including build-script output and dependency intermediate
  directories. A shallow `~/swbuild`-style checkout is the sanctioned convention,
  not a maximum enforced by this repository or the build box. There is
  deliberately no length fallback, retry, heuristic, or environment override.
- Tauri-generated capability schemas under `src-tauri/gen/schemas/` are ignored
  by the root ignore policy. Other generated or foreign files under
  `src-tauri/gen/` remain visible to source-state checks.
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
`swbuild.bundle` with an atomic `target/win-host-ci-source-binding.json` carrying
the exact snapshot commit and SHA-256 of both `Cargo.lock` and
`ui/package-lock.json`. The box bootstrap hard-checks it out under `~/swbuild`.
Before its first build, `scripts/win-ci.cmd` requires a clean checkout and all
three transferred values. The caller accepts only exactly one matching
`WIN_CI_HEAD`, `WIN_CI_CARGO_LOCK_SHA256`, and `WIN_CI_UI_LOCK_SHA256`, followed
by exactly one `WIN_CI_OK`; a missing, duplicate, stale, or mismatched
acknowledgement fails even when compilation was green.

When `EXPECTED_RELEASE_COMMIT` is set, synchronization enters release mode: it
refuses synthetic or dirty snapshots and transfers only the clean real commit
equal to that value. Candidate construction performs no fetch, `ls-remote`,
provider-CLI, or remote-ref lookup.
