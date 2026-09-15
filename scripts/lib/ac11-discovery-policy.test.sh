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

echo "ac11-discovery-policy tests passed"
