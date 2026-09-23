#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
POLICY=$SCRIPT_DIR/spl-journal-bridge-authority-policy.sh
TEST_ROOT=$(mktemp -d /var/tmp/spl-journal-bridge-policy.XXXXXX)
trap 'rm -rf "$TEST_ROOT"' EXIT

mkdir -p "$TEST_ROOT/crates/pl-transport-win/src" "$TEST_ROOT/crates/observer-pl/src"
cp "$ROOT/Cargo.toml" "$TEST_ROOT/Cargo.toml"
cp "$ROOT/Cargo.lock" "$TEST_ROOT/Cargo.lock"
cat > "$TEST_ROOT/crates/pl-transport-win/src/journal_bridge.rs" <<'EOF'
use spl_transport::journal_bridge::{self as shared_bridge, CarrierOpener, JournalBridgeConfig};
fn bridge() { let _ = shared_bridge::start; let _ = JournalBridgeConfig { opener: todo!(), bridge_names: todo!(), endpoint_hosts: vec![], policy: todo!() }; }
fn opener(client: &spl_transport::TransportClient) { let _ = client.open_carrier(None); }
EOF

sh "$POLICY" --root "$TEST_ROOT" >/dev/null
printf 'use observer_pl::frame::Frame;\n' > "$TEST_ROOT/crates/pl-transport-win/src/stale.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "stale copied import fixture unexpectedly passed" >&2
  exit 1
fi
rm "$TEST_ROOT/crates/pl-transport-win/src/stale.rs"
touch "$TEST_ROOT/crates/observer-pl/src/bridge.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "resurrected module fixture unexpectedly passed" >&2
  exit 1
fi
rm "$TEST_ROOT/crates/observer-pl/src/bridge.rs"
mkdir -p "$TEST_ROOT/crates/pl-transport-win/src/test_compat"
printf 'struct MuxCarrier;\n' > "$TEST_ROOT/crates/pl-transport-win/src/test_compat/red_mux.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "test_compat MuxCarrier fixture unexpectedly passed" >&2
  exit 1
fi
rm "$TEST_ROOT/crates/pl-transport-win/src/test_compat/red_mux.rs"
printf 'macro_rules! client_tests { () => {} }\n' > "$TEST_ROOT/crates/pl-transport-win/src/client.rs"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "generated AC11 client stub fixture unexpectedly passed" >&2
  exit 1
fi
rm "$TEST_ROOT/crates/pl-transport-win/src/client.rs"
sh "$POLICY" --root "$TEST_ROOT" >/dev/null

sed 's/rev = "72f6c1590a698194e689c230300d72c1b84ed9d4"/branch = "main"/' "$ROOT/Cargo.toml" > "$TEST_ROOT/Cargo.toml"
if sh "$POLICY" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "non-exact dependency fixture unexpectedly passed" >&2
  exit 1
fi

echo "spl-journal-bridge-authority-policy tests passed"
