#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

required=0.20.2
isolated_db_relative=target/release-advisory-db
cargo_bin=${CARGO:-cargo}
actual=$("$cargo_bin" deny --version 2>/dev/null | awk 'NR == 1 { print $2 }') || actual=
[ -n "$actual" ] || actual=unavailable

if [ "$actual" != "$required" ]; then
  echo "ERROR: cargo-deny version mismatch: expected $required, actual $actual. Run 'make provision-cargo-deny'. Offline release advisory checks use only $isolated_db_relative." >&2
  exit 1
fi
