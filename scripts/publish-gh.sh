#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Publish the Releases/ directory to GitHub Releases -- the REQUIRED source-hygiene
# mirror of every signed release (the R2 feed at updates.solstone.app is the primary
# auto-update channel; this is the download/source-of-record surface winget/scoop
# reference). Runs on the RELEASE HOST (where `gh` is authed + Releases/ was pulled),
# the same host + posture as publish-r2.sh -- NOT the build box (no `gh` there). There
# is no GitHub Actions release path by policy; the operator runs this by hand.
#
# Atomic-ish + fail-loud: `gh release create` errors if the tag already exists (no
# --clobber), so an un-bumped re-publish fails rather than silently overwriting. The
# feed JSON (releases.win.json) is uploaded LAST so clients never see a Setup.exe /
# nupkg without the matching feed. The release body carries the CHANGELOG.md
# "## [<version>]" section (same per-release notes as the R2 feed's NotesMarkdown).
#
# Requires: gh (authed to the repo), the repo's git remote (gh infers owner/name).

set -eu

RELEASES="${1:-Releases}"
REPO="${2:-}"            # optional "owner/name"; default: gh infers from the remote
CHANGELOG="CHANGELOG.md"

if [ ! -d "$RELEASES" ]; then
  echo "publish-gh: no Releases dir at '$RELEASES' -- pack on the box, then 'make pull-releases'." >&2
  exit 1
fi

# Preflight: fail fast if gh auth has degraded (same posture as publish-r2's wrangler check).
if ! gh auth status >/dev/null 2>&1; then
  echo "publish-gh: gh not authenticated -- run 'gh auth login', then retry." >&2
  exit 1
fi

repo_args=""
[ -n "$REPO" ] && repo_args="--repo $REPO"

# Version = the HIGHEST version among the packed full nupkgs (Releases/ accumulates
# every prior full/delta, so 'first' would wrongly pick the oldest). sort -V = semver.
VERSION="$(ls "$RELEASES"/Solstone-*-full.nupkg 2>/dev/null \
  | sed -E 's#.*/Solstone-(.+)-full\.nupkg#\1#; s/-win$//' \
  | sort -V | tail -1)"
if [ -z "$VERSION" ]; then
  echo "publish-gh: no full nupkg in '$RELEASES' -- not a packed release dir." >&2
  exit 1
fi
TAG="v$VERSION"

# Release notes: the CHANGELOG.md "## [<version>]" section body (mirrors package.ps1
# / the R2 feed's NotesMarkdown), written to a temp file for `gh --notes-file`.
# Falls back to a bare title line only if the section is absent.
notes_args="--notes Solstone $VERSION"
if [ -f "$CHANGELOG" ]; then
  notes_file="$(mktemp)"
  awk -v ver="$VERSION" '
    $0 ~ ("^## \\[" ver "\\]") { grab=1; next }
    grab && /^## \[/ { grab=0 }
    grab { print }
  ' "$CHANGELOG" | sed -e :a -e '/^\n*$/{$d;N;ba}' > "$notes_file"
  # Trim leading blank lines too.
  sed -i '/./,$!d' "$notes_file" 2>/dev/null || true
  if [ -s "$notes_file" ]; then
    notes_args="--notes-file $notes_file"
    echo "publish-gh: release notes from $CHANGELOG ## [$VERSION]"
  else
    echo "publish-gh: WARNING -- no '## [$VERSION]' section in $CHANGELOG; using bare title." >&2
  fi
fi

# Every asset except the feed JSON (uploaded last, below).
assets=""
for f in "$RELEASES"/*; do
  [ -f "$f" ] || continue
  [ "$(basename "$f")" = "releases.win.json" ] && continue
  assets="$assets \"$f\""
done

echo "publish-gh: creating GitHub release $TAG"
# Fail loud on an existing tag (no --clobber) -> the monotonic feed is never silently overwritten.
# shellcheck disable=SC2086
eval gh release create "$TAG" $repo_args --title "$TAG" $notes_args $assets

echo "publish-gh: uploading the update feed (releases.win.json) last"
# shellcheck disable=SC2086
gh release upload "$TAG" $repo_args "$RELEASES/releases.win.json"

# Sanity: the release must now exist.
# shellcheck disable=SC2086
gh release view "$TAG" $repo_args >/dev/null
echo "publish-gh: published $TAG."
