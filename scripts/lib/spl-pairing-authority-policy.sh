#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DEFAULT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$DEFAULT_ROOT

if [ "${1:-}" = "--root" ]; then
  if [ "$#" -lt 2 ]; then
    echo "spl-pairing-authority-policy: --root requires a directory" >&2
    exit 2
  fi
  ROOT=$2
  shift 2
fi

if ! ROOT=$(CDPATH= cd -- "$ROOT" 2>/dev/null && pwd); then
  echo "spl-pairing-authority-policy: repository root is unavailable" >&2
  exit 2
fi

violations=0
report_violation() {
  echo "spl-pairing-authority-policy: violation: $1" >&2
  violations=$((violations + 1))
}

# 1. Prohibited authority module files in crates/observer-pl/src/
prohibited_pl_files="pairlink.rs crockford.rs relay_window.rs wire.rs ca.rs"
for file in $prohibited_pl_files; do
  if [ -f "$ROOT/crates/observer-pl/src/$file" ]; then
    report_violation "found superseded authority file crates/observer-pl/src/$file; delete it in favor of spl-core"
  fi
done

# 2. Prohibited pairing ceremony symbols in observer-pl/src/
if [ -f "$ROOT/crates/observer-pl/src/lib.rs" ]; then
  if grep -qE "(DEFAULT_DIRECT_PORT|paths::PAIR|mod pairlink|mod crockford|mod relay_window|mod wire|mod ca)" "$ROOT/crates/observer-pl/src/lib.rs"; then
    report_violation "crates/observer-pl/src/lib.rs contains deleted module exports or pairing constants"
  fi
fi

# 3. Prohibited ceremony functions in pl-transport-win/src/pairing.rs
if [ -f "$ROOT/crates/pl-transport-win/src/pairing.rs" ]; then
  if grep -qE "(pub async fn pair\(|fn pair_with_seam|struct DirectPairingSeam|fn generate_material|fn credential_from_direct_pair_response)" "$ROOT/crates/pl-transport-win/src/pairing.rs"; then
    report_violation "crates/pl-transport-win/src/pairing.rs contains superseded local ceremony functions"
  fi
fi

# 4. Stale authority imports across crates and src-tauri
search_dirs=""
if [ -d "$ROOT/crates" ]; then
  search_dirs="$search_dirs $ROOT/crates"
fi
if [ -d "$ROOT/src-tauri" ]; then
  search_dirs="$search_dirs $ROOT/src-tauri"
fi

if [ -n "$search_dirs" ]; then
  stale_hits=$(find $search_dirs -path '*/src/test_compat/*' -prune -o -name "*.rs" -exec grep -HnE "(observer_pl::(pairlink|crockford|ca|relay_window|wire)|paths::PAIR|pair_dial_url)" {} + 2>/dev/null || true)
  if [ -n "$stale_hits" ]; then
    report_violation "found stale observer_pl authority references in Rust sources: $stale_hits"
  fi
fi

# 5. Dependency pins for spl-core and spl-transport
expected_rev="72f6c1590a698194e689c230300d72c1b84ed9d4"
expected_git="https://github.com/solpbc/spl-rust"

if [ -f "$ROOT/Cargo.toml" ]; then
  # Check for forbidden tag=, branch=, path= for spl-core or spl-transport
  if grep -E 'spl-(core|transport).*(\btag\b|\bbranch\b|\bpath\b)' "$ROOT/Cargo.toml" >/dev/null 2>&1; then
    report_violation "Cargo.toml contains forbidden tag, branch, or path dependency for spl-core or spl-transport"
  fi

  # Check exact git rev in Cargo.toml
  if ! grep -qE "spl-core.*git = \"$expected_git\".*rev = \"$expected_rev\"" "$ROOT/Cargo.toml"; then
    report_violation "root Cargo.toml missing spl-core pinned to git $expected_git and rev $expected_rev"
  fi
  if ! grep -qE "spl-transport.*git = \"$expected_git\".*rev = \"$expected_rev\"" "$ROOT/Cargo.toml"; then
    report_violation "root Cargo.toml missing spl-transport pinned to git $expected_git and rev $expected_rev"
  fi
fi

if [ -f "$ROOT/Cargo.lock" ]; then
  # Ensure lock contains spl-core and spl-transport with the exact git+https source
  expected_lock_source="git+$expected_git?rev=$expected_rev#$expected_rev"
  if ! grep -q "$expected_lock_source" "$ROOT/Cargo.lock"; then
    report_violation "Cargo.lock missing exact source $expected_lock_source"
  fi
fi

# 6. Live workspace check: verify cargo tree reaches spl-core and spl-transport in normal+build graph
if [ "$ROOT" = "$DEFAULT_ROOT" ]; then
  tree_output=$(cargo tree -p solstone-windows-app -e normal,build --locked 2>/dev/null || true)
  if ! echo "$tree_output" | grep -q "spl-core v0.1.0 ($expected_git?rev=$expected_rev"; then
    report_violation "cargo tree --locked solstone-windows-app normal,build graph is missing spl-core at rev $expected_rev"
  fi
  if ! echo "$tree_output" | grep -q "spl-transport v0.1.0 ($expected_git?rev=$expected_rev"; then
    report_violation "cargo tree --locked solstone-windows-app normal,build graph is missing spl-transport at rev $expected_rev"
  fi
fi

if [ "$violations" -ne 0 ]; then
  echo "spl-pairing-authority-policy: $violations policy violation(s) found" >&2
  exit 1
fi

echo "spl-pairing-authority-policy: all pairing authority policy checks passed"
