#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
REPO_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/.." && pwd)
RUSTC_BIN=${RUSTC:-rustc}

expected=$(
  sed -n 's/^[[:space:]]*channel[[:space:]]*=[[:space:]]*"\([^"]*\)".*$/\1/p' \
    "$REPO_ROOT/rust-toolchain.toml" | sed -n '1p'
)
actual=$("$RUSTC_BIN" -Vv 2>/dev/null | sed -n 's/^release:[[:space:]]*//p' | sed -n '1p') || actual=

[ -n "$expected" ] || expected=unavailable
[ -n "$actual" ] || actual=unavailable

if [ "$expected" = "unavailable" ] || [ "$actual" = "unavailable" ] || [ "$expected" != "$actual" ]; then
  echo "ERROR: Rust toolchain mismatch: expected $expected, actual $actual. Run 'make rust-toolchain'." >&2
  exit 1
fi
