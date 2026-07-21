#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
EXPECTED="ERROR: publication locked: direct publication is disabled; release publication belongs to the aggregate provenance publisher."
ASSERTIONS=0
TMP_ROOT=$(mktemp -d "${TMPDIR:-/tmp}/publication-guard.XXXXXX")
trap 'rm -rf "$TMP_ROOT"' EXIT HUP INT TERM

fail() {
  echo "publication-guard.test.sh: assertion failed: $1" >&2
  echo "publication-guard.test.sh: failure after $ASSERTIONS assertions" >&2
  exit 1
}

assert_eq() {
  if [ "$2" != "$3" ]; then
    echo "  expected: $2" >&2
    echo "  actual:   $3" >&2
    fail "$1"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

FAKE_BIN="$TMP_ROOT/bin"
WITNESS="$TMP_ROOT/transport-witness"
mkdir "$FAKE_BIN"
: > "$WITNESS"
for tool in gh wrangler curl jq scp; do
  fake="$FAKE_BIN/$tool"
  {
    echo '#!/usr/bin/env sh'
    echo 'echo invoked >> "$PUBLICATION_WITNESS"'
    echo 'exit 99'
  } > "$fake"
  chmod +x "$fake"
done

for script in publish-gh.sh publish-r2.sh publish-winget.sh publish-scoop.sh; do
  if output=$(PATH="$FAKE_BIN:$PATH" PUBLICATION_WITNESS="$WITNESS" sh "$REPO_ROOT/scripts/$script" ignored arbitrary arguments 2>&1); then
    fail "$script must fail closed"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  assert_eq "$script exact lockout" "$EXPECTED" "$output"
  assert_eq "$script no transport" "" "$(cat "$WITNESS")"
done

for target in publish publish-r2 publish-winget publish-scoop publish-packages; do
  : > "$WITNESS"
  if output=$(PATH="$FAKE_BIN:$PATH" PUBLICATION_WITNESS="$WITNESS" MAKEFLAGS= make -s -C "$REPO_ROOT" "$target" 2>&1); then
    fail "make $target must fail closed"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
  case "$output" in
    *"$EXPECTED"*) ASSERTIONS=$((ASSERTIONS + 1)) ;;
    *) fail "make $target exact lockout missing" ;;
  esac
  assert_eq "make $target no transport" "" "$(cat "$WITNESS")"
done

echo "publication-guard.test.sh: $ASSERTIONS assertions passed"
