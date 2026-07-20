#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

GIT=${GIT:-git}
SCP=${SCP:-scp}

phase=initialize
snapshot_sha=
did_create_swsync_ref=0
bundle_file=
sha_temp_file=

cleanup() {
  original_status=$1
  trap - EXIT HUP INT TERM
  set +e

  if [ "$did_create_swsync_ref" -eq 1 ]; then
    if ! "$GIT" update-ref -d refs/heads/__swsync "$snapshot_sha"; then
      echo "WARNING: sync-win-host: cleanup failed for refs/heads/__swsync; preserving $phase exit $original_status" >&2
    fi
  fi
  if [ -n "$bundle_file" ] && ! rm -f "$bundle_file"; then
    echo "WARNING: sync-win-host: cleanup failed for $bundle_file; preserving $phase exit $original_status" >&2
  fi
  if [ -n "$sha_temp_file" ] && ! rm -f "$sha_temp_file"; then
    echo "WARNING: sync-win-host: cleanup failed for $sha_temp_file; preserving $phase exit $original_status" >&2
  fi

  exit "$original_status"
}

trap 'cleanup $?' EXIT
trap 'cleanup 129' HUP
trap 'cleanup 130' INT
trap 'cleanup 143' TERM

fail_phase() {
  failed_phase=$1
  failed_status=$2
  echo "ERROR: sync-win-host: $failed_phase failed" >&2
  if [ "$failed_status" -eq 0 ]; then
    exit 1
  fi
  exit "$failed_status"
}

if script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd) &&
  repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd); then
  :
else
  fail_phase "$phase" "$?"
fi
WIN_CI_SHA_FILE=${WIN_CI_SHA_FILE:-"$repo_root/target/win-host-ci.sha"}
sha_dir=$(dirname -- "$WIN_CI_SHA_FILE")

if [ -z "${WIN_REMOTE_HOST:-}" ]; then
  echo "WIN_REMOTE_HOST is required" >&2
  fail_phase "$phase" 1
fi
if mkdir -p "$repo_root/target" "$sha_dir" && rm -f "$WIN_CI_SHA_FILE"; then
  :
else
  fail_phase "$phase" "$?"
fi
cd "$repo_root"

phase=guard
if GIT="$GIT" sh "$script_dir/check-win-sync-tree.sh"; then
  :
else
  fail_phase "$phase" "$?"
fi

phase=resolve-sha
if snapshot_sha=$("$GIT" stash create); then
  :
else
  fail_phase "$phase" "$?"
fi
if [ -z "$snapshot_sha" ]; then
  if snapshot_sha=$("$GIT" rev-parse HEAD); then
    :
  else
    fail_phase "$phase" "$?"
  fi
fi
case "$snapshot_sha" in
  "" | *[!0-9a-fA-F]*) fail_phase "$phase" 1 ;;
esac

phase=create-temp-ref
if "$GIT" update-ref refs/heads/__swsync "$snapshot_sha" ""; then
  did_create_swsync_ref=1
else
  fail_phase "$phase" "$?"
fi

phase=create-bundle
if bundle_file=$(mktemp "$repo_root/target/win-host-ci.bundle.XXXXXX") &&
  "$GIT" bundle create "$bundle_file" refs/heads/__swsync; then
  :
else
  fail_phase "$phase" "$?"
fi

phase=verify-bundle
if "$GIT" bundle verify "$bundle_file"; then
  :
else
  fail_phase "$phase" "$?"
fi
if bundle_heads=$("$GIT" bundle list-heads "$bundle_file"); then
  :
else
  fail_phase "$phase" "$?"
fi
if [ "$bundle_heads" != "$snapshot_sha refs/heads/__swsync" ]; then
  fail_phase "$phase" 1
fi

phase=delete-temp-ref
if "$GIT" update-ref -d refs/heads/__swsync "$snapshot_sha"; then
  did_create_swsync_ref=0
else
  fail_phase "$phase" "$?"
fi

phase=scp
if "$SCP" \
  -o ControlMaster=auto \
  -o "ControlPath=/tmp/sw-%r@%h:%p" \
  -o ControlPersist=60s \
  "$bundle_file" \
  "$WIN_REMOTE_HOST:swbuild.bundle"; then
  :
else
  fail_phase "$phase" "$?"
fi

phase=write-sha
if sha_temp_file=$(mktemp "$sha_dir/.win-host-ci.sha.XXXXXX") &&
  printf '%s\n' "$snapshot_sha" >"$sha_temp_file" &&
  mv "$sha_temp_file" "$WIN_CI_SHA_FILE"; then
  sha_temp_file=
else
  fail_phase "$phase" "$?"
fi

phase=success
echo "SYNC_WIN_HOST_OK sha=$snapshot_sha remote=swbuild.bundle"
