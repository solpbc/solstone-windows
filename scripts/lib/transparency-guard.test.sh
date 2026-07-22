#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

script_dir=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
repo_root=$(CDPATH= cd -- "$script_dir/../.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/transparency-guard.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

fail() {
  echo "transparency-guard.test.sh: assertion failed: $1" >&2
  exit 1
}

fake_bin="$temporary/bin"
witness="$temporary/transport-witness"
mkdir "$fake_bin"
: > "$witness"
for tool in gh wrangler scp; do
  fake="$fake_bin/$tool"
  {
    echo '#!/usr/bin/env sh'
    echo 'echo invoked >> "$TRANSPARENCY_GUARD_WITNESS"'
    echo 'exit 99'
  } > "$fake"
  chmod +x "$fake"
done

release_dir="$repo_root/xtask/tests/fixtures/rust-release-manifest/release-dir"
if output=$(env \
  -u TRANSPARENCY_BASE_URL \
  -u TRANSPARENCY_S3_ENDPOINT \
  -u TRANSPARENCY_BUCKET \
  -u TRANSPARENCY_S3_ACCESS_KEY_ID \
  -u TRANSPARENCY_S3_SECRET_ACCESS_KEY \
  -u TRANSPARENCY_MINISIGN_KEY \
  -u TRANSPARENCY_MINISIGN_PUB \
  -u TRANSPARENCY_ARCHIVE_CHANNEL \
  -u TRANSPARENCY_GENESIS \
  PATH="$fake_bin:$PATH" \
  TRANSPARENCY_GUARD_WITNESS="$witness" \
  MAKEFLAGS= \
  make -s -C "$repo_root" publish-transparency RELEASE_DIR="$release_dir" 2>&1); then
  fail "make publish-transparency must fail closed without configuration"
fi

expected="terminal transparency configuration: observed TRANSPARENCY_S3_ENDPOINT missing, expected all required publisher variables; restore the environment and retry"
case "$output" in
  *"$expected"*) ;;
  *) fail "actionable transparency diagnostic missing" ;;
esac
[ ! -s "$witness" ] || fail "a delivery command was invoked"

echo "transparency-guard.test.sh: fail-closed configuration boundary verified"
