#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
POLICY=$SCRIPT_DIR/ac11-discovery-policy.sh
CANDIDATE=$(mktemp -d /var/tmp/ac11-discovery-policy.XXXXXX)
trap 'rm -rf "$CANDIDATE"' EXIT

mkdir -p "$CANDIDATE/union"
cp "$ROOT/scripts/lib/fixtures/ac11-discovery/union/"*.txt "$CANDIDATE/union/"
sh "$POLICY" --root "$ROOT" --candidate "$CANDIDATE" >/dev/null
sed -i '1d' "$CANDIDATE/union/observer-pl.lib.txt"
if sh "$POLICY" --root "$ROOT" --candidate "$CANDIDATE" >/dev/null 2>&1; then
  echo "missing baseline case fixture unexpectedly passed" >&2
  exit 1
fi
cp "$ROOT/scripts/lib/fixtures/ac11-discovery/union/observer-pl.lib.txt" "$CANDIDATE/union/observer-pl.lib.txt"
sh "$POLICY" --root "$ROOT" --candidate "$CANDIDATE" >/dev/null

# The implementation check uses the committed exact-base lists as its only
# denominator; it does not harvest a replacement denominator after migration.
sh "$POLICY" --root "$ROOT" >/dev/null

# A complete-looking list followed by a failed Cargo command is still failure.
mkdir -p "$CANDIDATE/bin"
cat > "$CANDIDATE/bin/cargo" <<'EOF'
#!/bin/sh
set -eu
package=''; target=lib
while [ "$#" -gt 0 ]; do
  case "$1" in
    -p) package=$2; shift 2 ;;
    --test) target=$2; shift 2 ;;
    *) shift ;;
  esac
done
sed 's/$/: test/' "$AC11_BASE/$package.$target.txt"
exit 17
EOF
chmod +x "$CANDIDATE/bin/cargo"
if PATH="$CANDIDATE/bin:$PATH" AC11_BASE="$ROOT/scripts/lib/fixtures/ac11-discovery/linux" sh "$POLICY" --root "$ROOT" >/dev/null 2>&1; then
  echo "complete discovery with failed Cargo status unexpectedly passed" >&2
  exit 1
fi

echo "ac11-discovery-policy tests passed"
