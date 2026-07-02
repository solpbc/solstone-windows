#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

. "$(dirname "$0")/artifact-names.sh"

ASSERTIONS=0
TMP_ROOT=""

cleanup() {
  if [ -n "$TMP_ROOT" ]; then
    rm -rf "$TMP_ROOT"
  fi
}
trap cleanup EXIT HUP INT TERM

fail() {
  label="$1"
  expected="$2"
  actual="$3"
  echo "artifact-names.test.sh: assertion failed: $label" >&2
  echo "  expected: $expected" >&2
  echo "  actual:   $actual" >&2
  echo "artifact-names.test.sh: failure after $ASSERTIONS assertions" >&2
  exit 1
}

assert_eq() {
  label="$1"
  expected="$2"
  actual="$3"
  if [ "$actual" != "$expected" ]; then
    fail "$label" "$expected" "$actual"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

assert_cmp() {
  label="$1"
  expected_file="$2"
  actual_file="$3"
  if ! cmp -s "$expected_file" "$actual_file"; then
    fail "$label" "byte-identical to $expected_file" "$actual_file differs"
  fi
  ASSERTIONS=$((ASSERTIONS + 1))
}

TMP_ROOT="$(mktemp -d "${TMPDIR:-/tmp}/artifact-names.XXXXXX")"

assert_eq "setup_exe_name 0.2.7" "solstone-setup-0.2.7.exe" "$(setup_exe_name "0.2.7")"
assert_eq "setup_exe_name 1.10.0" "solstone-setup-1.10.0.exe" "$(setup_exe_name "1.10.0")"

assert_eq "winget_installer_url" \
  "https://github.com/solpbc/solstone-windows/releases/download/v0.2.7/solstone-setup-0.2.7.exe" \
  "$(winget_installer_url "solpbc/solstone-windows" "0.2.7")"

for version in 0.2.7 1.10.0; do
  dir="$TMP_ROOT/rename-$version"
  mkdir "$dir"
  default_setup="$dir/source-setup.bin"
  versioned_setup="$dir/$(setup_exe_name "$version")"
  head -c 4096 /dev/urandom > "$default_setup"
  cp "$default_setup" "$versioned_setup"
  assert_cmp "rename/copy byte identity $version" "$default_setup" "$versioned_setup"
done

release_dir="$TMP_ROOT/releases"
mkdir "$release_dir"
touch "$release_dir/Solstone-0.2.6-win-full.nupkg"
touch "$release_dir/Solstone-0.2.7-win-full.nupkg"
assert_eq "resolve_release_version" "0.2.7" "$(resolve_release_version "$release_dir")"

echo "artifact-names.test.sh: $ASSERTIONS assertions passed"
