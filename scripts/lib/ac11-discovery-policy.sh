#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu
# Committed fixtures are sorted bytewise.
export LC_ALL=C

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
CANDIDATE=""
while [ "$#" -gt 0 ]; do
  case "$1" in
    --candidate)
      [ "$#" -ge 2 ] || { echo "ac11-discovery-policy: --candidate requires a directory" >&2; exit 2; }
      CANDIDATE=$2
      shift 2
      ;;
    --root)
      [ "$#" -ge 2 ] || { echo "ac11-discovery-policy: --root requires a directory" >&2; exit 2; }
      ROOT=$2
      shift 2
      ;;
    *)
      echo "ac11-discovery-policy: unknown argument: $1" >&2
      exit 2
      ;;
  esac
done
ROOT=$(CDPATH= cd -- "$ROOT" 2>/dev/null && pwd) || exit 2
BASE="$ROOT/scripts/lib/fixtures/ac11-discovery"

fail() { echo "ac11-discovery-policy: $*" >&2; exit 1; }
[ -d "$BASE/linux" ] && [ -d "$BASE/windows" ] && [ -d "$BASE/union" ] || fail "baseline is incomplete"

for baseline in "$BASE/union"/*.txt; do
  name=$(basename "$baseline")
  union=$(mktemp /var/tmp/ac11-discovery-union.XXXXXX)
  trap 'rm -f "$union"' EXIT HUP INT TERM
  sort -u "$BASE/linux/$name" "$BASE/windows/$name" > "$union"
  cmp -s "$union" "$baseline" || fail "union baseline drift: $name"
  rm -f "$union"
  trap - EXIT HUP INT TERM
done

if [ -n "$CANDIDATE" ]; then
  for baseline in "$BASE/union"/*.txt; do
    name=$(basename "$baseline")
    [ -f "$CANDIDATE/union/$name" ] || fail "candidate missing $name"
    missing=$(comm -23 "$baseline" "$CANDIDATE/union/$name")
    [ -z "$missing" ] || fail "candidate dropped baseline cases from $name: $missing"
  done
  echo "ac11-discovery-policy: candidate is a baseline superset"
  exit 0
fi

for baseline in "$BASE/linux"/*.txt; do
  name=$(basename "$baseline")
  package=${name%%.*}
  target=${name#*.}
  target=${target%.txt}
  actual_file=$(mktemp /var/tmp/ac11-discovery-current.XXXXXX)
  raw_file=$(mktemp /var/tmp/ac11-discovery-raw.XXXXXX)
  names_file=$(mktemp /var/tmp/ac11-discovery-names.XXXXXX)
  trap 'rm -f "$actual_file" "$raw_file" "$names_file"' EXIT HUP INT TERM
  set --
  # Full discovery retains every baseline case, without executing fixtures.
  if [ "$package" = pl-transport-win ]; then
    set -- --features awaiting-hold,transport-tests
  fi
  if [ "$target" = lib ]; then
    cargo test --locked -p "$package" "$@" --lib -- --list > "$raw_file" || fail "Cargo discovery failed: $name"
  else
    cargo test --locked -p "$package" "$@" --test "$target" -- --list > "$raw_file" || fail "Cargo discovery failed: $name"
  fi
  sed -n 's/: test$//p' "$raw_file" > "$names_file"
  sort -u "$names_file" > "$actual_file"
  missing=$(comm -23 "$baseline" "$actual_file")
  rm -f "$actual_file" "$raw_file" "$names_file"
  trap - EXIT HUP INT TERM
  [ -z "$missing" ] || fail "Linux discovery dropped $name: $missing"
done

windows_only=$(mktemp /var/tmp/ac11-discovery-windows.XXXXXX)
trap 'rm -f "$windows_only"' EXIT HUP INT TERM
comm -13 "$BASE/linux/pl-transport-win.lib.txt" "$BASE/union/pl-transport-win.lib.txt" > "$windows_only"
while IFS= read -r case; do
  function=${case##*::}
  grep -R -q "fn $function" "$ROOT/crates/pl-transport-win/src" || fail "Windows-only baseline case missing: $case"
done < "$windows_only"
rm -f "$windows_only"
trap - EXIT HUP INT TERM

echo "ac11-discovery-policy: current discovery is a baseline superset"
