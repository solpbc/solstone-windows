#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Assert that the package-manager channels actually carry the current release.
#
# Channel publication is owned outside these direct scripts. winget once drifted
# TEN releases behind (0.2.0
# while we shipped 0.2.10) and nobody noticed, because a channel that is simply never
# updated emits no error: it just keeps serving the old version. Silence is not health.
# This turns that silence into a red check.
#
# Compares the live published version on each channel against the workspace version.
# Exit 1 on drift. Read-only -- it publishes nothing.
#
# It also fetches each committed manifest's own download URL and hashes the bytes,
# because a version match is not an installable package. Two real defects this
# catches, both found on the 2.0.7 train: the manifests carried 2.0.5-era digests
# (the bump advances version and URL, but the hash cannot exist until the signed
# artifact does, and nothing came back afterwards), and the GitHub release those
# URLs resolve from had not been cut since 2.0.5, so both channels pointed at a
# 404. Neither is visible from a version comparison.
#
#   make check-channels
set -eu

UPSTREAM="microsoft/winget-pkgs"
BUCKET="solpbc/scoop-solstone"

[ "$#" -eq 0 ] || { echo "check-channels: no arguments accepted" >&2; exit 2; }
VERSION="$(cargo run --locked -q -p xtask -- version-gate)"
[ -n "$VERSION" ] || { echo "check-channels: version gate returned no version" >&2; exit 1; }
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
  # Title match is prefix-agnostic on purpose: submissions appear as "New version:",
  # "Add version:" or "Update version:" depending on the tool and on whether an open PR
  # was retargeted to a later release. Pinning one form made a submitted PR read as DRIFT.
  PENDING="$(gh api -X GET search/issues \
             -f q="repo:$UPSTREAM is:pr is:open \"solpbc.Solstone version $VERSION\" in:title" \
             --jq '.items[].html_url' 2>/dev/null || true)"
  if [ -n "$PENDING" ]; then
    echo "  winget  PENDING  published $WINGET, $VERSION awaiting merge: $PENDING"
    pending=1
  else
    echo "  winget  DRIFT    published $WINGET, expected $VERSION -- submit a manifest PR to $UPSTREAM; see docs/release-runbook.md"
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
  echo "  scoop   DRIFT    published $SCOOP, expected $VERSION -- commit packaging/scoop/solstone.json into $BUCKET bucket/; our own bucket, no third-party queue"
  rc=1
fi

# The committed manifests are only inputs until their bytes resolve and match.
# A package manager fetches the URL and checks the digest; so does this.
check_asset() {
  label="$1"; url="$2"; want="$3"
  case "$want" in
    "" ) echo "  $label  UNKNOWN  no digest committed for $url"; rc=1; return ;;
  esac
  tmp="$(mktemp)" || { echo "  $label  UNKNOWN  mktemp failed"; rc=1; return; }
  if ! curl -sSfL --max-time 300 -o "$tmp" "$url" 2>/dev/null; then
    echo "  $label  MISSING  $url does not resolve -- the release the channel points at was never cut"
    rm -f "$tmp"; rc=1; return
  fi
  got="$(sha256sum "$tmp" | cut -d" " -f1)"
  rm -f "$tmp"
  want_lc="$(printf '%s' "$want" | tr 'A-F' 'a-f')"
  if [ "$got" = "$want_lc" ]; then
    echo "  $label  OK       digest matches the published bytes"
  else
    echo "  $label  STALE    committed $want_lc, published bytes are $got"
    rc=1
  fi
}

SCOOP_URL="$(sed -n 's/.*"url": *"\([^"]*\)".*/\1/p' packaging/scoop/solstone.json | head -1)"
SCOOP_HASH="$(sed -n 's/.*"hash": *"\([^"]*\)".*/\1/p' packaging/scoop/solstone.json | head -1)"
check_asset "scoop-bytes " "$SCOOP_URL" "$SCOOP_HASH"

WINGET_URL="$(sed -n 's/.*InstallerUrl: *\(.*\)/\1/p' packaging/winget/solpbc.Solstone.installer.yaml | head -1)"
WINGET_HASH="$(sed -n 's/.*InstallerSha256: *\(.*\)/\1/p' packaging/winget/solpbc.Solstone.installer.yaml | head -1)"
check_asset "winget-bytes" "$WINGET_URL" "$WINGET_HASH"

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
