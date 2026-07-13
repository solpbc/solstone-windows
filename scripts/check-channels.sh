#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Assert that the package-manager channels actually carry the current release.
#
# `make publish-packages` is an operator step on the release host -- nothing forces
# it, and when it fails it fails quietly. winget drifted TEN releases behind (0.2.0
# while we shipped 0.2.10) and nobody noticed, because a channel that is simply never
# updated emits no error: it just keeps serving the old version. Silence is not health.
# This turns that silence into a red check.
#
# Compares the live published version on each channel against the workspace version.
# Exit 1 on drift. Read-only -- it publishes nothing.
#
#   make check-channels              # against the workspace version
#   sh scripts/check-channels.sh 0.2.10
set -eu

UPSTREAM="microsoft/winget-pkgs"
BUCKET="solpbc/scoop-solstone"

VERSION="${1:-$(grep -m1 '^version = ' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')}"
[ -n "$VERSION" ] || { echo "check-channels: could not determine VERSION" >&2; exit 1; }
command -v gh >/dev/null 2>&1 || { echo "check-channels: gh required (and authed)" >&2; exit 1; }

echo "check-channels: workspace version $VERSION"
rc=0
pending=0

# winget: highest version directory merged under the package path.
WINGET="$(gh api "repos/$UPSTREAM/contents/manifests/s/solpbc/Solstone" \
          --jq '[.[] | select(.type=="dir") | .name] | .[]' 2>/dev/null \
          | sort -V | tail -1 || true)"
if [ -z "$WINGET" ]; then
  echo "  winget  UNKNOWN  (no manifests found under solpbc/Solstone)"
  rc=1
elif [ "$WINGET" = "$VERSION" ]; then
  echo "  winget  OK       $WINGET"
else
  PENDING="$(gh api -X GET search/issues \
             -f q="repo:$UPSTREAM is:pr is:open \"New version: solpbc.Solstone version $VERSION\" in:title" \
             --jq '.items[].html_url' 2>/dev/null || true)"
  if [ -n "$PENDING" ]; then
    echo "  winget  PENDING  published $WINGET, $VERSION awaiting merge: $PENDING"
    pending=1
  else
    echo "  winget  DRIFT    published $WINGET, expected $VERSION -- run: make publish-winget"
    rc=1
  fi
fi

# scoop: the bucket manifest's version.
SCOOP="$(gh api "repos/$BUCKET/contents/bucket/solstone.json" --jq '.content' 2>/dev/null \
         | base64 -d | sed -n 's/.*"version": *"\([^"]*\)".*/\1/p' | head -1 || true)"
if [ -z "$SCOOP" ]; then
  echo "  scoop   UNKNOWN  (could not read $BUCKET bucket/solstone.json)"
  rc=1
elif [ "$SCOOP" = "$VERSION" ]; then
  echo "  scoop   OK       $SCOOP"
else
  echo "  scoop   DRIFT    published $SCOOP, expected $VERSION -- run: make publish-scoop"
  rc=1
fi

if [ "$rc" -ne 0 ]; then
  echo "check-channels: DRIFT -- a channel is serving a stale version." >&2
elif [ "$pending" -ne 0 ]; then
  # Not drift (the PR is open), but NOT current either -- say so. A channel that is
  # merely awaiting merge is still serving the old version to users today.
  echo "check-channels: submitted, awaiting merge -- not yet live on every channel."
else
  echo "check-channels: all channels current."
fi
exit "$rc"
