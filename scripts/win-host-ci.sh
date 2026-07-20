#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

GIT=${GIT:-git}
SCP=${SCP:-scp}
SSH=${SSH:-ssh}
ssh_output_file=

cleanup() {
  original_status=$1
  trap - EXIT HUP INT TERM
  set +e
  if [ -n "$ssh_output_file" ]; then
    rm -f "$ssh_output_file"
  fi
  exit "$original_status"
}

trap 'cleanup $?' EXIT
trap 'cleanup 129' HUP
trap 'cleanup 130' INT
trap 'cleanup 143' TERM

if ! command -v flock >/dev/null 2>&1; then
  echo "ERROR: win-host-ci: flock is required on the Linux driver host but was not found" >&2
  exit 1
fi

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/.." && pwd)
WIN_CI_SHA_FILE=${WIN_CI_SHA_FILE:-"$repo_root/target/win-host-ci.sha"}
cd "$repo_root"

if git_common_dir=$("$GIT" rev-parse --git-common-dir); then
  :
else
  echo "ERROR: win-host-ci: unable to resolve git common directory" >&2
  exit 1
fi
case "$git_common_dir" in
  /*) ;;
  *) git_common_dir=$repo_root/$git_common_dir ;;
esac
if git_common_dir=$(CDPATH= cd -- "$git_common_dir" && pwd); then
  :
else
  echo "ERROR: win-host-ci: unable to resolve git common directory" >&2
  exit 1
fi
lock_path=$git_common_dir/solstone-win-host-ci.lock

exec 9>"$lock_path"
echo "win-host-ci: waiting for lock $lock_path"
flock 9
echo "win-host-ci: acquired lock $lock_path"

if WIN_REMOTE_HOST="${WIN_REMOTE_HOST:-}" \
  WIN_CI_SHA_FILE="$WIN_CI_SHA_FILE" \
  GIT="$GIT" \
  SCP="$SCP" \
  sh "$script_dir/sync-win-host.sh"; then
  :
else
  sync_status=$?
  echo "ERROR: win-host-ci: sync failed" >&2
  exit "$sync_status"
fi

if [ -f "$WIN_CI_SHA_FILE" ]; then
  sha_line_count=$(awk 'END { print NR }' "$WIN_CI_SHA_FILE")
  snapshot_sha=$(sed -n '1p' "$WIN_CI_SHA_FILE")
else
  sha_line_count=0
  snapshot_sha=
fi
case "$snapshot_sha" in
  "" | *[!0-9a-fA-F]*) sha_file_valid=0 ;;
  *) sha_file_valid=1 ;;
esac
if [ "$sha_line_count" -ne 1 ] || [ "$sha_file_valid" -ne 1 ]; then
  if [ "$WIN_CI_SHA_FILE" = "$repo_root/target/win-host-ci.sha" ]; then
    sha_file_display=target/win-host-ci.sha
  else
    sha_file_display=$WIN_CI_SHA_FILE
  fi
  echo "ERROR: win-host-ci: snapshot SHA file is missing or malformed: $sha_file_display" >&2
  exit 1
fi

ssh_output_file=$(mktemp "$repo_root/target/win-host-ci.ssh.XXXXXX")
if "$SSH" \
  -o ControlMaster=auto \
  -o "ControlPath=/tmp/sw-%r@%h:%p" \
  -o ControlPersist=60s \
  "${WIN_REMOTE_HOST:-}" \
  'cmd /c C:\sol\sw-ci.cmd' >"$ssh_output_file"; then
  ssh_status=0
else
  ssh_status=$?
fi
cat "$ssh_output_file"
if [ "$ssh_status" -ne 0 ]; then
  echo "ERROR: win-host-ci: ssh failed (exit $ssh_status)" >&2
  exit "$ssh_status"
fi

normalized_output=$(awk '{ sub(/\r$/, ""); print }' "$ssh_output_file")
head_count=$(printf '%s\n' "$normalized_output" | awk '/^WIN_CI_HEAD=/ { count++ } END { print count + 0 }')
if [ "$head_count" -ne 1 ]; then
  echo "ERROR: win-host-ci: expected exactly one WIN_CI_HEAD line, found $head_count" >&2
  exit 1
fi
remote_head=$(printf '%s\n' "$normalized_output" | sed -n 's/^WIN_CI_HEAD=//p')
if [ "$remote_head" != "$snapshot_sha" ]; then
  echo "ERROR: win-host-ci: remote HEAD mismatch: expected $snapshot_sha, actual $remote_head" >&2
  exit 1
fi
if ! printf '%s\n' "$normalized_output" | grep -q '^=== WIN_CI_OK:'; then
  echo "ERROR: win-host-ci: WIN_CI_OK acknowledgement missing" >&2
  exit 1
fi

echo "WIN_HOST_CI_VERIFIED sha=$snapshot_sha"
