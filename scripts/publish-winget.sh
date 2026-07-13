#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Open a winget version-update PR (solpbc.Solstone -> microsoft/winget-pkgs) for a
# published release. Runs on the RELEASE HOST after `make publish` -- the GitHub
# release and its solstone-setup-<version>.exe asset must already exist, because the
# manifest is hashed over the PUBLISHED asset (what users actually download).
#
# THE MANIFEST SOURCE OF TRUTH IS packaging/winget/, IN THIS REPO. This script
# renders those three YAML files (substituting the per-release version / installer
# URL / SHA256 / release date, and deriving ReleaseNotes from CHANGELOG.md) and
# opens the PR from them.
#
# It used to shell out to `komac update`, and that was the bug. `komac update` is a
# version bumper: it pulls the LAST PUBLISHED manifest from winget-pkgs, changes the
# version/url/hash, and re-submits. It never reads packaging/winget/. So every field
# that is not version-shaped -- PackageName, ShortDescription, Description, Tags,
# Dependencies, AppsAndFeaturesEntries -- silently carried forward from the first
# package, and corrections made in this repo could never reach the listing. They sat
# unpublished from 0.2.0 through 0.2.10. (komac also had to be installed on the
# release host; it was not, so this step exited 1 and was skipped in silence.)
#
# It also mis-detects Architecture: it sniffs the PE header of Velopack's 32-bit
# Setup.exe stub and writes x86, though the app it installs is x86-64. See the note
# in packaging/winget/solpbc.Solstone.installer.yaml.
#
# winget has no push API -- every version is a PR to the community repo -- so this
# builds the branch through the GitHub API (no multi-GB clone of winget-pkgs) and
# opens the PR. winget's own pipeline then validates (schema, hash, an interactive
# Windows-Sandbox install) before a moderator/bot merges: the PR is itself the
# install-validation gate.
#
# Operator-driven, no CI path -- the same posture as publish-gh.sh / publish-r2.sh.
# Requires `gh` (authed, with a fork of microsoft/winget-pkgs) + curl + jq.
# VERSION defaults to the workspace package version; pass an arg to override.
#
#   WINGET_DRY_RUN=1 sh scripts/publish-winget.sh 0.2.11
#
# renders the manifests to WINGET_OUT (default target/winget/) and stops -- no repo
# write, no branch, no PR. Validate them before submitting:
#   winget validate --manifest target/winget/     (on Windows)
set -eu

. "$(dirname "$0")/lib/artifact-names.sh"

REPO="solpbc/solstone-windows"
PKG="solpbc.Solstone"
UPSTREAM="microsoft/winget-pkgs"
SRCDIR="packaging/winget"

VERSION="${1:-$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')}"
[ -n "$VERSION" ] || { echo "publish-winget: could not determine VERSION (pass it as an arg)" >&2; exit 1; }
TAG="v$VERSION"
URL="$(winget_installer_url "$REPO" "$VERSION")"
MANIFEST_DIR="manifests/s/solpbc/Solstone/$VERSION"
BRANCH="solpbc-Solstone-$VERSION"
DRY_RUN="${WINGET_DRY_RUN:-}"
OUT="${WINGET_OUT:-target/winget}"

for f in solpbc.Solstone.yaml solpbc.Solstone.installer.yaml solpbc.Solstone.locale.en-US.yaml; do
  [ -f "$SRCDIR/$f" ] || { echo "publish-winget: missing manifest source $SRCDIR/$f" >&2; exit 1; }
done
command -v gh   >/dev/null 2>&1 || { echo "publish-winget: gh required (and authed)" >&2; exit 1; }
command -v jq   >/dev/null 2>&1 || { echo "publish-winget: jq required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "publish-winget: curl required" >&2; exit 1; }

if [ -z "$DRY_RUN" ]; then
  # Already merged? Then there is nothing to open.
  if gh api "repos/$UPSTREAM/contents/$MANIFEST_DIR" >/dev/null 2>&1; then
    echo "publish-winget: $PKG $VERSION is already merged upstream ($MANIFEST_DIR) -- nothing to do."
    exit 0
  fi

  # One PR per version -- never open a duplicate against the community repo. Use the
  # SEARCH api, not /pulls: winget-pkgs carries ~1000 open PRs, so a paged listing
  # only reliably sees a PR while it is still one of the newest -- i.e. it would stop
  # finding ours exactly when a re-run days later needs it to.
  EXISTING="$(gh api -X GET search/issues \
              -f q="repo:$UPSTREAM is:pr is:open \"New version: $PKG version $VERSION\" in:title" \
              --jq '.items[].html_url' 2>/dev/null || true)"
  [ -z "$EXISTING" ] || { echo "publish-winget: a PR for $PKG $VERSION is already open: $EXISTING" >&2; exit 1; }

  FORK_OWNER="$(gh api user --jq '.login')"
  FORK="$FORK_OWNER/winget-pkgs"
  gh repo view "$FORK" >/dev/null 2>&1 || {
    echo "publish-winget: no fork at $FORK -- run: gh repo fork $UPSTREAM --clone=false" >&2; exit 1; }
fi

echo "publish-winget: $PKG -> $VERSION (from $SRCDIR)"

# Hash the PUBLISHED asset (fail loud if the release isn't up yet). winget wants
# the SHA256 uppercased.
TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
curl -fsSL "$URL" -o "$TMP/setup.exe" || {
  echo "publish-winget: failed to fetch $URL -- is the release published?" >&2; exit 1; }
SHA="$(sha256sum "$TMP/setup.exe" | cut -d' ' -f1 | tr 'a-f' 'A-F')"
RELEASE_DATE="$(gh api "repos/$REPO/releases/tags/$TAG" --jq '.published_at' | cut -dT -f1)"
echo "publish-winget: $URL"
echo "publish-winget: sha256=$SHA  releaseDate=$RELEASE_DATE"

# Render the repo manifests at this version, and write the per-release scalars back
# into the repo so the committed source stays a faithful mirror of what is published.
for f in solpbc.Solstone.yaml solpbc.Solstone.installer.yaml solpbc.Solstone.locale.en-US.yaml; do
  sed -e "s|^PackageVersion: .*|PackageVersion: $VERSION|" \
      -e "s|^ReleaseDate: .*|ReleaseDate: $RELEASE_DATE|" \
      -e "s|^\( *InstallerUrl:\) .*|\1 $URL|" \
      -e "s|^\( *InstallerSha256:\) .*|\1 $SHA|" \
      "$SRCDIR/$f" > "$TMP/$f"
  [ -n "$DRY_RUN" ] || cp "$TMP/$f" "$SRCDIR/$f"
done

# ReleaseNotes are DERIVED, not authored: pull the CHANGELOG.md "## [<version>]"
# section (the same source as the Velopack pack notes and the R2 feed) and splice it
# into the locale manifest as a YAML block scalar. Markdown heading markers are
# stripped ("### Fixed" -> "Fixed") -- winget renders ReleaseNotes as plain text, so
# a literal "###" would show up in the listing.
NOTES="$(awk -v v="$VERSION" '
  $0 ~ "^## \\[" v "\\]" { inb=1; next }
  inb && /^## \[/ { exit }
  inb { sub(/^#+[[:space:]]*/, ""); sub(/[[:space:]]+$/, ""); print }
' CHANGELOG.md | sed -e '/./,$!d' | tac | sed -e '/./,$!d' | tac)"

if [ -n "$NOTES" ]; then
  # Splice before ManifestType so ManifestType/ManifestVersion stay last (the
  # winget-pkgs house order).
  awk -v notes="$NOTES" -v url="https://github.com/$REPO/releases/tag/$TAG" '
    /^ManifestType:/ && !spliced {
      print "ReleaseNotes: |-"
      n = split(notes, lines, "\n")
      for (i = 1; i <= n; i++) print (lines[i] == "" ? "" : "  " lines[i])
      print "ReleaseNotesUrl: " url
      spliced = 1
    }
    { print }
  ' "$TMP/solpbc.Solstone.locale.en-US.yaml" > "$TMP/locale.spliced"
  mv "$TMP/locale.spliced" "$TMP/solpbc.Solstone.locale.en-US.yaml"
else
  echo "publish-winget: WARNING no CHANGELOG.md '## [$VERSION]' section -- publishing without release notes." >&2
fi

if [ -n "$DRY_RUN" ]; then
  mkdir -p "$OUT"
  for f in solpbc.Solstone.yaml solpbc.Solstone.installer.yaml solpbc.Solstone.locale.en-US.yaml; do
    cp "$TMP/$f" "$OUT/$f"
  done
  echo "publish-winget: DRY RUN -- rendered $MANIFEST_DIR to $OUT/ (no repo write, no branch, no PR)."
  exit 0
fi

# Build the PR branch through the API (winget-pkgs is far too large to clone).
gh repo sync "$FORK" >/dev/null 2>&1 || true
BASE="$(gh api "repos/$FORK/git/ref/heads/master" --jq '.object.sha')"
UPSTREAM_HEAD="$(gh api "repos/$UPSTREAM/git/ref/heads/master" --jq '.object.sha')"
[ "$BASE" = "$UPSTREAM_HEAD" ] || {
  echo "publish-winget: fork $FORK is not in sync with $UPSTREAM master -- sync it and retry." >&2; exit 1; }
BASETREE="$(gh api "repos/$FORK/git/commits/$BASE" --jq '.tree.sha')"

TREE_ITEMS=""
for f in solpbc.Solstone.yaml solpbc.Solstone.installer.yaml solpbc.Solstone.locale.en-US.yaml; do
  BLOB="$(gh api -X POST "repos/$FORK/git/blobs" \
          -f content="$(base64 -w0 "$TMP/$f")" -f encoding=base64 --jq '.sha')"
  TREE_ITEMS="$TREE_ITEMS$(jq -nc --arg p "$MANIFEST_DIR/$f" --arg s "$BLOB" \
                 '{path:$p, mode:"100644", type:"blob", sha:$s}')
"
done
TREE="$(printf '%s' "$TREE_ITEMS" | jq -sc --arg bt "$BASETREE" '{base_tree:$bt, tree:.}' \
        | gh api -X POST "repos/$FORK/git/trees" --input - --jq '.sha')"

# Tree-completeness assertion -- NON-NEGOTIABLE. If base_tree is dropped (an empty
# var, a flag-parsing quirk, an API change), the new tree contains ONLY our files and
# the commit silently becomes a deletion of the entire winget-pkgs repository. GitHub
# then can't even render the diff, so the winget bot never labels the PR and it sits
# dead -- or worse, a human moderator opens a PR that nukes 4M files. This happened
# (PR #401803, 2026-07-13, hand-driven -- caught before the pipeline saw it). The
# root tree entry count must match the base's exactly: we only ever add files UNDER
# the existing manifests/ tree, never at the root.
ROOT_N="$(gh api "repos/$FORK/git/trees/$TREE" --jq '.tree | length')"
BASE_N="$(gh api "repos/$FORK/git/trees/$BASETREE" --jq '.tree | length')"
[ "$ROOT_N" = "$BASE_N" ] || {
  echo "publish-winget: FATAL tree incomplete -- root has $ROOT_N entries, base has $BASE_N." >&2
  echo "  base_tree was dropped building the tree; refusing to commit a repo-wide deletion." >&2
  exit 1; }

COMMIT="$(jq -nc --arg m "New version: $PKG version $VERSION" --arg t "$TREE" --arg p "$BASE" \
          '{message:$m, tree:$t, parents:[$p]}' \
          | gh api -X POST "repos/$FORK/git/commits" --input - --jq '.sha')"

if gh api "repos/$FORK/git/ref/heads/$BRANCH" >/dev/null 2>&1; then
  gh api -X PATCH "repos/$FORK/git/refs/heads/$BRANCH" -f sha="$COMMIT" -F force=true --jq '.object.sha' >/dev/null
else
  gh api -X POST "repos/$FORK/git/refs" -f ref="refs/heads/$BRANCH" -f sha="$COMMIT" --jq '.object.sha' >/dev/null
fi
echo "publish-winget: pushed $FORK@$BRANCH ($COMMIT)"

gh pr create --repo "$UPSTREAM" --base master --head "$FORK_OWNER:$BRANCH" \
  --title "New version: $PKG version $VERSION" \
  --body "Version update for \`$PKG\` to \`$VERSION\`, published by the package's own author (sol pbc).

Installer: [\`$(setup_exe_name "$VERSION")\`](https://github.com/$REPO/releases/tag/$TAG) — SHA256 verified against the published asset.

- [x] Have you signed the [Contributor License Agreement](https://cla.opensource.microsoft.com/microsoft/winget-pkgs)?
- [x] Have you checked that there aren't other open [pull requests](https://github.com/$UPSTREAM/pulls) for the same manifest update/change?
- [x] Does your manifest conform to the [1.12 schema](https://github.com/$UPSTREAM/tree/master/doc/manifest/schema/1.12.0)?"

echo "publish-winget: PR opened -- winget CI validates (interactive-sandbox install), then a moderator merges."
echo "publish-winget: commit the re-rendered $SRCDIR/ so the repo mirrors what shipped."
