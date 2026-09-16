#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DEFAULT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$DEFAULT_ROOT

if [ "${1:-}" = "--root" ]; then
  [ "$#" -ge 2 ] || { echo "spl-journal-bridge-authority-policy: --root requires a directory" >&2; exit 2; }
  ROOT=$2
fi
ROOT=$(CDPATH= cd -- "$ROOT" 2>/dev/null && pwd) || exit 2

violations=0
violation() {
  echo "spl-journal-bridge-authority-policy: violation: $1" >&2
  violations=$((violations + 1))
}

# The pairing policy is the sole owner of the exact shared revision.
sh "$SCRIPT_DIR/spl-pairing-authority-policy.sh" --root "$ROOT" >/dev/null || violation "shared dependency pin is not exact"

for manifest in "$ROOT/Cargo.toml" "$ROOT"/*/Cargo.toml "$ROOT"/*/*/Cargo.toml; do
  [ -f "$manifest" ] || continue
  shared_override=$(grep -nE '(spl-(core|transport).*(path|patch|tag|branch|vendor)|\[patch\.)' "$manifest" 2>/dev/null || true)
  [ -z "$shared_override" ] || violation "non-exact shared dependency form in $manifest: $shared_override"
done

if [ -f "$ROOT/Cargo.lock" ]; then
  expected_source="git+https://github.com/solpbc/spl-rust?rev=fb82d70cd60eac83a5633d12511a4a8a102e26dd#fb82d70cd60eac83a5633d12511a4a8a102e26dd"
  for package in spl-core spl-transport; do
    source=$(awk -v package="$package" '
      $0 == "[[package]]" { in_package = 0 }
      $0 ~ "^name = \"" package "\"$" { in_package = 1 }
      in_package && /^source = / { sub(/^source = "/, ""); sub(/"$/, ""); print; exit }
    ' "$ROOT/Cargo.lock")
    [ "$source" = "$expected_source" ] || violation "Cargo.lock $package source is not the pinned shared revision"
  done
fi

for relative in \
  crates/observer-pl/src/bridge.rs crates/observer-pl/src/frame.rs \
  crates/observer-pl/src/http.rs crates/observer-pl/src/jwt.rs \
  crates/observer-pl/src/mux.rs crates/observer-pl/src/relay.rs \
  crates/observer-pl/src/relay_access.rs \
  crates/pl-transport-win/src/connection.rs \
  crates/pl-transport-win/src/journal_bridge_carrier.rs \
  crates/pl-transport-win/src/tls.rs crates/pl-transport-win/src/relay.rs \
  crates/pl-transport-win/src/relay_http.rs \
  crates/pl-transport-win/src/relay_token.rs \
  crates/pl-transport-win/src/spki_pin.rs; do
  [ ! -f "$ROOT/$relative" ] || violation "deleted authority file resurrected: $relative"
done

search_roots=""
for relative in crates/pl-transport-win/src crates/pl-transport-win/tests crates/pl-transport-win/examples src-tauri/src; do
  [ -d "$ROOT/$relative" ] && search_roots="$search_roots $ROOT/$relative"
done
if [ -n "$search_roots" ]; then
  stale=$(find $search_roots -path '*/src/test_compat/*' -prune -o -name '*.rs' -exec grep -HnE 'observer_pl::(bridge|frame|http|jwt|mux|relay|relay_access)' {} + 2>/dev/null || true)
  [ -z "$stale" ] || violation "stale copied observer-pl authority import: $stale"
  stale=$(find $search_roots -path '*/src/test_compat/*' -prune -o -name '*.rs' -exec grep -HnE 'pl_transport_win::(connection|tls|relay|relay_http|relay_token|journal_bridge_carrier|spki_pin)(::|;)' {} + 2>/dev/null || true)
  [ -z "$stale" ] || violation "stale deleted Windows transport import: $stale"
fi

production="$ROOT/crates/pl-transport-win/src"
if [ -d "$production" ]; then
  forbidden=$(find "$production" -path '*/test_compat/*' -prune -o -name '*.rs' -exec grep -HnE 'TransportClient::dial_carrier|MuxCarrier|disconnect_relay_if_active' {} + 2>/dev/null || true)
  [ -z "$forbidden" ] || violation "local carrier authority remains: $forbidden"
  stubbed=$(grep -HnF 'macro_rules! client_tests' "$production/client.rs" 2>/dev/null || true)
  [ -z "$stubbed" ] || violation "AC11 client behavior collapsed into generated name-only stubs: $stubbed"
fi

# AC10 applies to test compatibility modules too: they may call shared APIs, but
# must never become a second implementation of their authority.
if [ -d "$ROOT/crates" ]; then
  copied=$(find "$ROOT/crates" -name '*.rs' -exec grep -HnE '^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?struct[[:space:]]+MuxCarrier([[:space:]]|<|\{|;|$)' {} + 2>/dev/null || true)
  [ -z "$copied" ] || violation "local MuxCarrier implementation remains: $copied"
  copied=$(find "$ROOT/crates" -name '*.rs' -exec grep -HnE '^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?fn[[:space:]]+parse_request_head[[:space:]]*\(' {} + 2>/dev/null || true)
  [ -z "$copied" ] || violation "local parse_request_head implementation remains: $copied"
  copied=$(find "$ROOT/crates/pl-transport-win/src" -name '*.rs' -exec grep -HnE '^[[:space:]]*(pub([[:space:]]*\([^)]*\))?[[:space:]]+)?fn[[:space:]]+pairing_config[[:space:]]*\(' {} + 2>/dev/null || true)
  [ -z "$copied" ] || violation "local pairing_config implementation remains: $copied"
fi

bridge="$ROOT/crates/pl-transport-win/src/journal_bridge.rs"
if [ ! -f "$bridge" ]; then
  violation "Windows journal bridge adapter is missing"
else
  for needle in 'JournalBridgeConfig' 'CarrierOpener' 'shared_bridge::start' '.open_carrier('; do
    grep -F -q "$needle" "$bridge" || violation "adapter lacks $needle"
  done
fi

if [ "$violations" -ne 0 ]; then
  exit 1
fi
echo "spl-journal-bridge-authority-policy: all journal bridge authority policy checks passed"
