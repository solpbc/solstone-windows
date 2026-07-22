#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

fail() {
  echo "ERROR: minisign gate failed: $1" >&2
  exit 1
}

if ! command -v minisign >/dev/null 2>&1; then
  echo "ERROR: minisign is required; run cargo install minisign --locked" >&2
  exit 1
fi

version=$(minisign -v 2>&1) || fail "the installed binary did not report its version"
case "$version" in
  "minisign 0.11"|"minisign 0.12") ;;
  *) fail "observed $version, expected minisign 0.11 or 0.12" ;;
esac

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
temporary=$(mktemp -d "${TMPDIR:-/tmp}/solstone-minisign-gate.XXXXXX")
trap 'rm -rf "$temporary"' EXIT HUP INT TERM

public_key="$temporary/throwaway.pub"
secret_key="$temporary/transparency-test-secret"
body="$root/xtask/tests/fixtures/transparency/entry-vector.canonical.json"
signature="$temporary/entry.minisig"
tampered="$temporary/tampered.json"

minisign -G -W -p "$public_key" -s "$secret_key" >/dev/null 2>&1 || fail "throwaway key generation failed"
minisign -S -s "$secret_key" -m "$body" -x "$signature" -t "solpbc-transparency-v1 gate" >/dev/null 2>&1 || fail "fixture signing failed"
minisign -V -p "$public_key" -m "$body" -x "$signature" >/dev/null 2>&1 || fail "fixture verification failed"

cp "$body" "$tampered" || fail "tamper fixture copy failed"
printf 'x' >> "$tampered" || fail "tamper fixture write failed"
if minisign -V -p "$public_key" -m "$tampered" -x "$signature" >/dev/null 2>&1; then
  fail "tampered fixture verified"
fi

echo "minisign gate: version accepted, throwaway signature verified, tamper rejected"
