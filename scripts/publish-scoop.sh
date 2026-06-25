#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Refresh the scoop manifest (solpbc/scoop-solstone : bucket/solstone.json) to a
# published release. Runs on the RELEASE HOST after `make publish` -- the GitHub
# release and its Solstone-win-Portable.zip asset must already exist, because the
# manifest hash is computed over the PUBLISHED asset (what users actually download).
#
# Operator-driven, no CI path -- the same posture as publish-gh.sh / publish-r2.sh.
# VERSION defaults to the workspace package version; pass an arg to override.
set -eu

REPO="solpbc/solstone-windows"
BUCKET="solpbc/scoop-solstone"
MANIFEST="bucket/solstone.json"

VERSION="${1:-$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')}"
[ -n "$VERSION" ] || { echo "publish-scoop: could not determine VERSION (pass it as an arg)" >&2; exit 1; }
TAG="v$VERSION"
URL="https://github.com/$REPO/releases/download/$TAG/Solstone-win-Portable.zip"

command -v gh   >/dev/null 2>&1 || { echo "publish-scoop: gh required (and authed)" >&2; exit 1; }
command -v jq   >/dev/null 2>&1 || { echo "publish-scoop: jq required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "publish-scoop: curl required" >&2; exit 1; }

echo "publish-scoop: $BUCKET -> solstone $VERSION"

# Hash the PUBLISHED asset (fail loud if the release isn't up yet).
SHA="$(curl -fsSL "$URL" | sha256sum | cut -d' ' -f1)"
[ -n "$SHA" ] || { echo "publish-scoop: failed to fetch/hash $URL -- is the release published?" >&2; exit 1; }
echo "publish-scoop: $URL  sha256=$SHA"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT
gh api "repos/$BUCKET/contents/$MANIFEST" --jq '.content' | base64 -d > "$TMP/cur.json"
BLOB_SHA="$(gh api "repos/$BUCKET/contents/$MANIFEST" --jq '.sha')"
jq --arg v "$VERSION" --arg h "$SHA" --arg u "$URL" \
   '.version=$v | .architecture."64bit".url=$u | .architecture."64bit".hash=$h' \
   "$TMP/cur.json" > "$TMP/new.json"

if cmp -s "$TMP/cur.json" "$TMP/new.json"; then
  echo "publish-scoop: manifest already at $VERSION -- nothing to do."
  exit 0
fi

gh api -X PUT "repos/$BUCKET/contents/$MANIFEST" \
  -f message="solstone $VERSION" \
  -f sha="$BLOB_SHA" \
  -f content="$(base64 -w0 "$TMP/new.json")" --jq '.commit.sha' >/dev/null
echo "publish-scoop: pushed solstone $VERSION to $BUCKET ($MANIFEST)."
