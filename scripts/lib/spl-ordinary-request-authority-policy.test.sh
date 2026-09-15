#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
POLICY=$SCRIPT_DIR/spl-ordinary-request-authority-policy.sh
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
TEST_ROOT=$(mktemp -d /var/tmp/spl-ordinary-policy.XXXXXX)
cleanup() { rm -rf "$TEST_ROOT"; }
trap cleanup EXIT HUP INT TERM

# Green: the real tree must satisfy the policy.
sh "$POLICY"

# Red/green falsification: copy only the policy inputs, plant a bypass, prove
# rejection, remove it, and prove the isolated fixture is green again.
mkdir -p "$TEST_ROOT/crates/pl-transport-win/src" "$TEST_ROOT/crates/observer-pl/src" "$TEST_ROOT/src-tauri/src"
cp "$ROOT/Cargo.toml" "$ROOT/Cargo.lock" "$TEST_ROOT/"
cp "$ROOT/crates/pl-transport-win/src/ordinary_request.rs" "$TEST_ROOT/crates/pl-transport-win/src/"
cp "$ROOT/crates/pl-transport-win/src/client.rs" "$TEST_ROOT/crates/pl-transport-win/src/"
for file in post_connect.rs coordinator.rs journal_version.rs; do touch "$TEST_ROOT/crates/pl-transport-win/src/$file"; done
mkdir -p "$TEST_ROOT/crates/pl-transport-win/src/integration"
touch "$TEST_ROOT/crates/pl-transport-win/src/integration/ops.rs"
touch "$TEST_ROOT/crates/pl-transport-win/src/journal_bridge.rs"
touch "$TEST_ROOT/crates/observer-pl/src/lib.rs"

sh "$POLICY" --root "$TEST_ROOT" >/dev/null
printf '%s\n' 'async fn send(&self) {}' >> "$TEST_ROOT/crates/pl-transport-win/src/client.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure for obsolete ObserverClient::send" >&2
  exit 1
fi
sed -i '$d' "$TEST_ROOT/crates/pl-transport-win/src/client.rs"
sh "$POLICY" --root "$TEST_ROOT" >/dev/null

printf '%s\n' '/app/network/api/clients/self' >> "$TEST_ROOT/crates/pl-transport-win/src/journal_bridge.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure for an ordinary route in the bridge" >&2
  exit 1
fi
sed -i '$d' "$TEST_ROOT/crates/pl-transport-win/src/journal_bridge.rs"
sh "$POLICY" --root "$TEST_ROOT" >/dev/null

echo "spl-ordinary-request-authority-policy tests passed"
