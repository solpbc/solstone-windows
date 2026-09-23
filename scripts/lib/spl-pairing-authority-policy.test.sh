#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
POLICY_SCRIPT=$SCRIPT_DIR/spl-pairing-authority-policy.sh

TEST_ROOT=$(mktemp -d "${TMPDIR:-/var/tmp}/spl-policy-test.XXXXXX")
cleanup() {
  rm -rf "$TEST_ROOT"
}
trap cleanup EXIT HUP INT TERM

# Test 1: Real repository passes policy cleanly
sh "$POLICY_SCRIPT"

# Helper to populate a clean passing mock fixture in TEST_ROOT
setup_clean_fixture() {
  rm -rf "$TEST_ROOT"
  mkdir -p "$TEST_ROOT/crates/observer-pl/src"
  mkdir -p "$TEST_ROOT/crates/pl-transport-win/src"
  mkdir -p "$TEST_ROOT/src-tauri/src"
  cat <<'EOF' > "$TEST_ROOT/Cargo.toml"
[workspace.dependencies]
spl-core = { git = "https://github.com/solpbc/spl-rust", rev = "72f6c1590a698194e689c230300d72c1b84ed9d4" }
spl-transport = { git = "https://github.com/solpbc/spl-rust", rev = "72f6c1590a698194e689c230300d72c1b84ed9d4" }
EOF
  cat <<'EOF' > "$TEST_ROOT/Cargo.lock"
[[package]]
name = "spl-core"
version = "0.1.0"
source = "git+https://github.com/solpbc/spl-rust?rev=72f6c1590a698194e689c230300d72c1b84ed9d4#72f6c1590a698194e689c230300d72c1b84ed9d4"

[[package]]
name = "spl-transport"
version = "0.1.0"
source = "git+https://github.com/solpbc/spl-rust?rev=72f6c1590a698194e689c230300d72c1b84ed9d4#72f6c1590a698194e689c230300d72c1b84ed9d4"
EOF
  touch "$TEST_ROOT/crates/observer-pl/src/lib.rs"
  touch "$TEST_ROOT/crates/pl-transport-win/src/pairing.rs"
}

# Test 2: Clean mock fixture passes
setup_clean_fixture
sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null

# Test 3: Fails when superseded file exists in crates/observer-pl/src/
setup_clean_fixture
touch "$TEST_ROOT/crates/observer-pl/src/pairlink.rs"
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when pairlink.rs exists" >&2
  exit 1
fi

# Test 4: Fails when a stale import exists in a Rust source file
setup_clean_fixture
cat <<'EOF' > "$TEST_ROOT/crates/pl-transport-win/src/stale.rs"
use observer_pl::pairlink::parse;
EOF
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when stale observer_pl::pairlink import exists" >&2
  exit 1
fi

# Test 5: Fails when Cargo.toml uses tag= instead of rev=
setup_clean_fixture
cat <<'EOF' > "$TEST_ROOT/Cargo.toml"
[workspace.dependencies]
spl-core = { git = "https://github.com/solpbc/spl-rust", tag = "v0.7.2" }
spl-transport = { git = "https://github.com/solpbc/spl-rust", rev = "72f6c1590a698194e689c230300d72c1b84ed9d4" }
EOF
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when tag= is used in Cargo.toml" >&2
  exit 1
fi

# Test 6: Fails when Cargo.toml uses branch=
setup_clean_fixture
cat <<'EOF' > "$TEST_ROOT/Cargo.toml"
[workspace.dependencies]
spl-core = { git = "https://github.com/solpbc/spl-rust", branch = "main" }
spl-transport = { git = "https://github.com/solpbc/spl-rust", rev = "72f6c1590a698194e689c230300d72c1b84ed9d4" }
EOF
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when branch= is used in Cargo.toml" >&2
  exit 1
fi

# Test 7: Fails when Cargo.toml uses path=
setup_clean_fixture
cat <<'EOF' > "$TEST_ROOT/Cargo.toml"
[workspace.dependencies]
spl-core = { path = "../spl-core" }
spl-transport = { git = "https://github.com/solpbc/spl-rust", rev = "72f6c1590a698194e689c230300d72c1b84ed9d4" }
EOF
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when path= is used in Cargo.toml" >&2
  exit 1
fi

# Test 8: Fails when Cargo.lock is missing exact rev source
setup_clean_fixture
cat <<'EOF' > "$TEST_ROOT/Cargo.lock"
[[package]]
name = "spl-core"
version = "0.1.0"
source = "registry+https://github.com/rust-lang/crates.io-index"
EOF
if sh "$POLICY_SCRIPT" --root "$TEST_ROOT" >/dev/null 2>&1; then
  echo "expected policy failure when Cargo.lock source is wrong" >&2
  exit 1
fi

echo "spl-pairing-authority-policy tests passed"
