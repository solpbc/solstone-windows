#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Publish the Velopack Releases/ directory to the R2 update feed at
# updates.solstone.app/solstone-windows/ -- the auto-update channel the in-app
# velopack::UpdateManager fetches. This is the PRIMARY update feed; the GitHub
# release (scripts/publish-gh.sh) is the required source-hygiene mirror.
#
# Runs on the RELEASE HOST (where wrangler holds the Cloudflare R2 auth for the
# solstone-updates bucket), NOT on the build box -- mirrors the macOS appcast
# publish split, which keeps Cloudflare credentials off the signing box. Pack on
# the box (`make package`), pull Releases/ here (`make pull-releases`), then run
# this (`make publish-r2`).
#
# Atomic-ish + fail-loud: the feed JSON (releases.win.json) is uploaded LAST, so
# an update client never sees a feed referencing a nupkg/Setup.exe that is not yet
# on R2. Requires: wrangler (authed to the Cloudflare account), curl.

set -eu

. "$(dirname "$0")/lib/artifact-names.sh"

BUCKET="solstone-updates"
PREFIX="solstone-windows"
BASE_URL="https://updates.solstone.app"
FEED="releases.win.json"
# Cloudflare account id. `wrangler whoami` must list this -- catches a silently
# degraded OAuth token before any upload (the ~24h OAuth decay; same gate the
# macOS appcast publish uses).
CF_ACCOUNT_ID="3f2c1528c7d4d9685819ea9e9e307c92"

RELEASES="${1:-Releases}"
if [ ! -d "$RELEASES" ]; then
  echo "publish-r2: no Releases dir at '$RELEASES' -- pack on the box, then 'make pull-releases'." >&2
  exit 1
fi
if [ ! -f "$RELEASES/$FEED" ]; then
  echo "publish-r2: no $FEED in '$RELEASES' -- not a Velopack output dir." >&2
  exit 1
fi
VERSION="$(resolve_release_version "$RELEASES")"
if [ -z "$VERSION" ]; then
  echo "publish-r2: could not resolve a version from full nupkgs in '$RELEASES'" >&2
  exit 1
fi
SETUP="$(setup_exe_name "$VERSION")"

# Preflight: fail fast if wrangler's Cloudflare auth has degraded. whoami
# exercises the same account-lookup path the uploads need.
who="$(wrangler whoami 2>&1 || true)"
case "$who" in
  *"$CF_ACCOUNT_ID"*) : ;;
  *)
    echo "publish-r2: wrangler Cloudflare auth degraded (account $CF_ACCOUNT_ID not in 'wrangler whoami') -- run 'wrangler login', then retry." >&2
    exit 1
    ;;
esac

content_type_for() {
  case "$1" in
    *.nupkg) echo "application/octet-stream" ;;
    *.exe)   echo "application/octet-stream" ;;
    *.zip)   echo "application/zip" ;;
    *.json)  echo "application/json" ;;
    *)       echo "text/plain" ;;
  esac
}

put_one() {
  f="$1"
  name="$(basename "$f")"
  ct="$(content_type_for "$name")"
  echo "publish-r2: put $PREFIX/$name ($ct)"
  # no-cache: releases.win.json and Solstone-win-Portable.zip are stable names
  # reused across releases, so a Cloudflare edge cache entry from a prior upload
  # can serve stale bytes to real downloads until its TTL expires. Found live
  # 2026-07-02 when a fresh browser download installed stale bytes after a newer
  # release was published and verified at the origin. Versioning the setup
  # artifact is the structural fix for that case; no-cache remains correct for
  # the still-stable names and harmless for immutable versioned artifacts.
  wrangler r2 object put "$BUCKET/$PREFIX/$name" --file="$f" --remote --content-type="$ct" --cache-control="no-cache"
}

# Everything except the feed, first.
for f in "$RELEASES"/*; do
  [ -f "$f" ] || continue
  [ "$(basename "$f")" = "$FEED" ] && continue
  put_one "$f"
done

# The feed LAST -- so the client never resolves a feed ahead of its assets.
put_one "$RELEASES/$FEED"

# Sanity: the feed + the versioned setup artifact must be reachable.
feed_url="$BASE_URL/$PREFIX/$FEED"
setup_url="$BASE_URL/$PREFIX/$SETUP"
for u in "$feed_url" "$setup_url"; do
  code="$(curl -sS -I -o /dev/null -w '%{http_code}' "$u" || echo 000)"
  if [ "$code" != "200" ]; then
    echo "publish-r2: HEAD $u returned HTTP $code (expected 200)." >&2
    exit 1
  fi
done

echo "publish-r2: published."
echo "publish-r2: feed:      $feed_url"
echo "publish-r2: setup:     $setup_url"
echo "publish-r2: permalink: https://solstone.app/download/windows"
