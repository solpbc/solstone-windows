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
  # These Phase-8 documents must be scanned before their first commit too.
  for candidate in \
    docs/release-finalizer-design.md \
    docs/release-finalizer-witness-ledger.md
  do
    if [ -f "$ROOT/$candidate" ]; then
      printf '%s\n' "$candidate" >> "$LIST"
    fi
  done
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
      printf "%s:%d:%s:%s:%s\n", file, start, rule, compact(paragraph), remediation
    }
    function flush(   lower, authority, authority_feed, authority_qualified,
                          required_signal, mirror_linked, mirror_required,
                          mirror_qualified, cargo_position, vpk_position, tail,
                          publication_after, chained, prohibited) {
      if (paragraph == "") return
      lower = tolower(paragraph)

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
      chained = (cargo_position > 0 && vpk_position > cargo_position) || (vpk_position > 0 && publication_after)
      prohibited = lower ~ /(never|do not|must not|forbid|fail-closed|disabled)/
      if (chained && (code_fence || imperative) && !prohibited) {
        report("hand-chain", "replace raw command chaining with make package/finalizer and the aggregate provenance publisher")
      }

      paragraph = ""
      start = 0
      code_fence = 0
      imperative = 0
    }
    /^[[:space:]]*$/ { flush(); next }
    {
      if (paragraph == "") start = NR
      paragraph = paragraph (paragraph == "" ? "" : " ") $0
      line = tolower($0)
      if (line ~ /```/ || line ~ /~~~/) code_fence = 1
      gsub(/`/, "", line)
      if (line ~ /^[[:space:]]*([-*][[:space:]]+|[0-9]+\.[[:space:]]+|[$>][[:space:]]*)?(run[[:space:]]+)?(cargo[[:space:]]+build|vpk[[:space:]]+pack)/) {
        imperative = 1
      }
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
