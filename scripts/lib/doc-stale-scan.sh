#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DEFAULT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$DEFAULT_ROOT

if [ "${1:-}" = "--root" ]; then
  if [ "$#" -lt 2 ]; then
    echo "doc-stale-scan: --root requires a directory" >&2
    exit 2
  fi
  ROOT=$2
  shift 2
fi

if ! ROOT=$(CDPATH= cd -- "$ROOT" 2>/dev/null && pwd); then
  echo "doc-stale-scan: documentation root is unavailable; restore the checkout and retry" >&2
  exit 2
fi

LIST=$(mktemp "${TMPDIR:-/tmp}/doc-stale-scan.XXXXXX")
SORTED=$(mktemp "${TMPDIR:-/tmp}/doc-stale-scan-sorted.XXXXXX")
cleanup() {
  rm -f "$LIST" "$SORTED"
}
trap cleanup EXIT HUP INT TERM

add_candidate() {
  candidate=$1
  case "$candidate" in
    /*)
      case "$candidate" in
        "$ROOT"/*) candidate=${candidate#"$ROOT"/} ;;
        *)
          echo "doc-stale-scan: input is outside the documentation root; pass a checkout-relative Markdown path" >&2
          exit 2
          ;;
      esac
      ;;
  esac
  case "$candidate" in
    ../*|*/../*|*/..|.)
      echo "doc-stale-scan: input escapes the documentation root; pass a checkout-relative Markdown path" >&2
      exit 2
      ;;
  esac
  case "$candidate" in
    *.md|AGENTS.md) printf '%s\n' "$candidate" >> "$LIST" ;;
    *)
      echo "doc-stale-scan: ineligible input '$candidate'; scan Markdown documentation only" >&2
      exit 2
      ;;
  esac
}

if [ "$#" -gt 0 ]; then
  for candidate in "$@"; do
    add_candidate "$candidate"
  done
else
  if git -C "$ROOT" rev-parse --is-inside-work-tree >/dev/null 2>&1; then
    git -C "$ROOT" ls-files -- '*.md' 'AGENTS.md' | LC_ALL=C sort -u >> "$LIST"
  fi
fi

LC_ALL=C sort -u "$LIST" > "$SORTED"
scanned=0
violations=0

while IFS= read -r relative; do
  [ -n "$relative" ] || continue
  full=$ROOT/$relative
  if [ ! -f "$full" ]; then
    echo "$relative:1:scan-input:eligible documentation is missing:restore the tracked file or remove the stale index entry" >&2
    violations=$((violations + 1))
    continue
  fi
  scanned=$((scanned + 1))
  findings=$(awk -v file="$relative" '
    function compact(value) {
      gsub(/[[:space:]]+/, " ", value)
      sub(/^ /, "", value)
      sub(/ $/, "", value)
      return value
    }
    function report(rule, remediation) {
      printf "%s:%d:%s:%s:%s\n", file, start, rule, compact(unit), remediation
    }
    function flush(   lower, authority, authority_feed, authority_qualified,
                          required_signal, mirror_linked, mirror_required,
                          mirror_qualified, cargo_position, vpk_position, tail,
                          publication_after, publication_here, chained, prohibited,
                          command_unit) {
      if (unit == "") return
      lower = tolower(unit)

      authority_feed = lower ~ /authoritative/ || lower ~ /primary[^.!?]*feed/ || lower ~ /update[[:space:]]+feed/ || lower ~ /serves?[^.!?]*feed/ || lower ~ /hosts?[^.!?]*feed/
      authority = lower ~ /github/ && authority_feed
      authority_qualified = lower ~ /(optional|non-authoritative|non authoritative|secondary)/ || lower ~ /(not|never|no)[^.!?]*(authoritative|primary[^.!?]*feed|update[[:space:]]+feed|serves?[^.!?]*feed|hosts?[^.!?]*feed)/ || lower ~ /(does not|cannot)[^.!?]*(serve|host)[^.!?]*feed/
      if (authority && !authority_qualified) {
        report("github-authority", "name R2 as authoritative and qualify any GitHub mirror as optional and non-authoritative")
      }

      required_signal = lower ~ /(^|[^[:alpha:]])(required|blocks?|gates?)([^[:alpha:]]|$)/ || lower ~ /must[[:space:]]+succeed/ || lower ~ /cannot[[:space:]]+release/
      mirror_linked = lower ~ /mirror[^.!?]*(([^[:alpha:]])(required|blocks?|gates?)([^[:alpha:]])|must[[:space:]]+succeed|cannot[[:space:]]+release)/ || lower ~ /((^|[^[:alpha:]])(required|blocks?|gates?)([^[:alpha:]]|$)|must[[:space:]]+succeed|cannot[[:space:]]+release)[^.!?]*mirror/
      mirror_required = (lower ~ /github/ && required_signal) || mirror_linked
      mirror_qualified = lower ~ /(optional|non-authoritative|non authoritative)/ || lower ~ /(not|never|no)[^.!?]*((^|[^[:alpha:]])(required|blocks?|gates?)([^[:alpha:]]|$)|must[[:space:]]+succeed|cannot[[:space:]]+release)/ || lower ~ /cannot[[:space:]]+gate/ || lower ~ /does not[^.!?]*(block|gate|require)/ || lower ~ /must[[:space:]]+not/
      if (mirror_required && !mirror_qualified) {
        report("required-mirror", "state that the GitHub mirror is optional, non-authoritative, and cannot gate release")
      }

      cargo_position = index(lower, "cargo build")
      vpk_position = index(lower, "vpk pack")
      publication_after = 0
      if (vpk_position > 0) {
        tail = substr(lower, vpk_position + length("vpk pack"))
        publication_after = tail ~ /gh[[:space:]]+release/ || tail ~ /r2[[:space:]]+upload/ || tail ~ /wrangler[^.!?]*r2[^.!?]*(put|upload)/ || tail ~ /(^|[^[:alpha:]])publish([^[:alpha:]]|$)/ || tail ~ /(^|[^[:alpha:]])upload([^[:alpha:]]|$)/
      }
      publication_here = lower ~ /gh[[:space:]]+release/ || lower ~ /r2[[:space:]]+upload/ || lower ~ /wrangler[^.!?]*r2[^.!?]*(put|upload)/ || lower ~ /(^|[^[:alpha:]])publish([^[:alpha:]]|$)/ || lower ~ /(^|[^[:alpha:]])upload([^[:alpha:]]|$)/
      chained = (cargo_position > 0 && vpk_position > cargo_position) || (vpk_position > 0 && publication_after) || (sequence_cargo && vpk_position > 0) || (sequence_vpk && publication_here)
      prohibited = lower ~ /(never|do not|must not|forbid|fail-closed|disabled)/
      command_unit = code_fence || imperative || publication_imperative
      if (chained && command_unit && !prohibited) {
        report("hand-chain", "replace raw command chaining with make package/finalizer and the aggregate provenance publisher")
      }

      if (prohibited || !command_unit) {
        sequence_cargo = 0
        sequence_vpk = 0
      } else {
        if (cargo_position > 0) sequence_cargo = 1
        if (vpk_position > 0) sequence_vpk = 1
      }
      unit = ""
      start = 0
      code_fence = 0
      imperative = 0
      publication_imperative = 0
    }
    function append_line(value,   line) {
      if (unit == "") start = NR
      unit = unit (unit == "" ? "" : " ") value
      line = tolower(value)
      gsub(/`/, "", line)
      if (line ~ /^[[:space:]]*([-*+][[:space:]]+|[0-9]+[.)][[:space:]]+|[$>][[:space:]]*)?(run[[:space:]]+)?(cargo[[:space:]]+build|vpk[[:space:]]+pack)/) imperative = 1
      if (line ~ /^[[:space:]]*([-*+][[:space:]]+|[0-9]+[.)][[:space:]]+|[$>][[:space:]]*)?(run[[:space:]]+)?(gh[[:space:]]+release|r2[[:space:]]+upload|wrangler[^[:space:]]*[[:space:]]+r2|publish|upload)/) publication_imperative = 1
    }
    /^[[:space:]]*(```|~~~)/ {
      if (!in_fence) {
        flush()
        sequence_cargo = 0
        sequence_vpk = 0
        in_fence = 1
        code_fence = 1
        append_line($0)
      } else {
        append_line($0)
        code_fence = 1
        flush()
        in_fence = 0
        sequence_cargo = 0
        sequence_vpk = 0
      }
      next
    }
    in_fence { code_fence = 1; append_line($0); next }
    /^[[:space:]]*$/ { flush(); sequence_cargo = 0; sequence_vpk = 0; next }
    /^[[:space:]]*\|.*\|[[:space:]]*$/ { flush(); append_line($0); flush(); next }
    /^[[:space:]]*([-*+][[:space:]]+|[0-9]+[.)][[:space:]]+)/ { flush(); append_line($0); next }
    /^[[:space:]]*#+[[:space:]]+/ { flush(); append_line($0); flush(); sequence_cargo = 0; sequence_vpk = 0; next }
    {
      append_line($0)
    }
    END { flush() }
  ' "$full")
  if [ -n "$findings" ]; then
    printf '%s\n' "$findings" >&2
    count=$(printf '%s\n' "$findings" | awk 'END { print NR }')
    violations=$((violations + count))
  fi
done < "$SORTED"

if [ "$scanned" -eq 0 ]; then
  echo "doc-stale-scan: scanned zero eligible files; restore tracked Markdown inputs or pass explicit files" >&2
  exit 1
fi

if [ "$violations" -ne 0 ]; then
  echo "doc-stale-scan: scanned $scanned eligible files; found $violations stale instruction violation(s)" >&2
  exit 1
fi

echo "doc-stale-scan: scanned $scanned eligible files; no violations"
