#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Refresh the scoop manifest (solpbc/scoop-solstone : bucket/solstone.json) to a
# published release. Runs on the RELEASE HOST after `make publish` -- the GitHub
# release and its Solstone-win-Portable.zip asset must already exist, because the
# manifest hash is computed over the PUBLISHED asset (what users actually download).
#
# THE MANIFEST SOURCE OF TRUTH IS packaging/scoop/solstone.json, IN THIS REPO.
# This script renders it (substituting the per-release version/url/hash) and pushes
# the WHOLE file to the bucket. It does NOT patch the live manifest in place.
#
# That is deliberate, and it is a bug fix. This script used to fetch the live
# manifest and jq-patch only .version/.url/.hash -- so every other field silently
# carried forward from whatever was first published, and nothing in this repo could
# ever correct it. Two live defects came from exactly that:
#   - the `bin`/`shortcuts` kept pointing at `Solstone.exe` after the 2026-07-03
#     brand sweep renamed the Velopack --packTitle to `sol` (the portable zip's
#     launcher became `sol.exe`), so `scoop install solstone` broke in 0.2.9/0.2.10;
#   - the `description` kept retired product vocabulary long after the copy was
#     corrected here.
# Edit the manifest in this repo; the release ships it. See packaging/DISTRIBUTION.md.
#
# Operator-driven, no CI path -- the same posture as publish-gh.sh / publish-r2.sh.
# VERSION defaults to the workspace package version; pass an arg to override.
set -eu

REPO="solpbc/solstone-windows"
BUCKET="solpbc/scoop-solstone"
MANIFEST="bucket/solstone.json"
SRC="packaging/scoop/solstone.json"

VERSION="${1:-$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')}"
[ -n "$VERSION" ] || { echo "publish-scoop: could not determine VERSION (pass it as an arg)" >&2; exit 1; }
TAG="v$VERSION"
URL="https://github.com/$REPO/releases/download/$TAG/Solstone-win-Portable.zip"

[ -f "$SRC" ] || { echo "publish-scoop: missing manifest source $SRC" >&2; exit 1; }
command -v gh   >/dev/null 2>&1 || { echo "publish-scoop: gh required (and authed)" >&2; exit 1; }
command -v jq   >/dev/null 2>&1 || { echo "publish-scoop: jq required" >&2; exit 1; }
command -v curl >/dev/null 2>&1 || { echo "publish-scoop: curl required" >&2; exit 1; }

echo "publish-scoop: $BUCKET -> solstone $VERSION (from $SRC)"

# Hash the PUBLISHED asset (fail loud if the release isn't up yet).
SHA="$(curl -fsSL "$URL" | sha256sum | cut -d' ' -f1)"
[ -n "$SHA" ] || { echo "publish-scoop: failed to fetch/hash $URL -- is the release published?" >&2; exit 1; }
echo "publish-scoop: $URL  sha256=$SHA"

TMP="$(mktemp -d)"; trap 'rm -rf "$TMP"' EXIT

# Render the repo manifest at this version. Write it back to the repo too, so the
# committed source stays a faithful mirror of what is published (commit it).
jq --arg v "$VERSION" --arg h "$SHA" --arg u "$URL" \
   '.version=$v | .architecture."64bit".url=$u | .architecture."64bit".hash=$h' \
   "$SRC" > "$TMP/new.json"
cp "$TMP/new.json" "$SRC"

gh api "repos/$BUCKET/contents/$MANIFEST" --jq '.content' | base64 -d > "$TMP/cur.json"
BLOB_SHA="$(gh api "repos/$BUCKET/contents/$MANIFEST" --jq '.sha')"

if cmp -s "$TMP/cur.json" "$TMP/new.json"; then
  echo "publish-scoop: bucket already matches $SRC at $VERSION -- nothing to do."
  exit 0
fi

gh api -X PUT "repos/$BUCKET/contents/$MANIFEST" \
  -f message="solstone $VERSION" \
  -f sha="$BLOB_SHA" \
  -f content="$(base64 -w0 "$TMP/new.json")" --jq '.commit.sha' >/dev/null
echo "publish-scoop: pushed solstone $VERSION to $BUCKET ($MANIFEST)."
echo "publish-scoop: commit the re-rendered $SRC so the repo mirrors what shipped."
