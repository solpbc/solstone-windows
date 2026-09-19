#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc
#
# Provision (or reuse) a checkout-local Cargo home whose advisory-dbs
# directory holds ONLY the approved private RustSec mirror, so
# check-release-advisory-config's "exactly one contained mirror repository"
# gate never trips on a shared dev host that also carries cargo-deny's
# ordinary public default cache under the real Cargo home. This is the same
# "provision a clean cargo home containing only the approved private RustSec
# mirror cache" step docs/release-runbook.md already asks an operator to do
# by hand; this script makes it reproducible and idempotent instead of a
# scratch directory a session has to remember how to rebuild.
#
# The mirror clone is fetched with `cargo deny fetch db` against a minimal,
# locator-only deny.toml so cargo-deny names the on-disk clone exactly the
# way check-release-advisory-config's own cargo-deny invocation expects
# (a hash of the db URL) -- this script never guesses that naming itself.
#
# The real Cargo home (toolchain, bin, registry cache, credentials) is shared
# in read-only via symlink; only advisory-dbs is ever repo-local and
# mirror-only. The mirror locator is supplied by the caller and never
# committed (SOLSTONE_ADVISORY_MIRROR_LOCATOR is required elsewhere too).
#
# Prints the resulting Cargo home path on stdout; every other message goes to
# stderr so callers can safely capture "$(...)" this script's output.

set -eu

mirror_locator="${SOLSTONE_ADVISORY_MIRROR_LOCATOR:-}"
if [ -z "$mirror_locator" ]; then
  echo "ERROR: SOLSTONE_ADVISORY_MIRROR_LOCATOR is required; set it to the approved private mirror Git URL and retry." >&2
  exit 1
fi

cargo_bin=${CARGO:-cargo}
repo_root=$(git rev-parse --show-toplevel)
cargo_home="$repo_root/target/advisory-cargo-home"
db_root="$cargo_home/advisory-dbs"
real_cargo_home="${CARGO_HOME:-$HOME/.cargo}"
locator_marker="$cargo_home/.advisory-mirror-locator"

mkdir -p "$db_root"

for shared in bin git registry env .crates.toml .crates2.json; do
  link="$cargo_home/$shared"
  source="$real_cargo_home/$shared"
  if [ ! -e "$link" ] && [ ! -L "$link" ] && [ -e "$source" ]; then
    ln -s "$source" "$link"
  fi
done

needs_fetch=1
if [ -f "$locator_marker" ] && [ "$(cat "$locator_marker")" = "$mirror_locator" ] \
  && find "$db_root" -mindepth 1 -maxdepth 1 -type d -print -quit | grep -q .; then
  needs_fetch=0
fi

if [ "$needs_fetch" -eq 1 ]; then
  echo "advisory-cargo-home.sh: provisioning approved mirror into $db_root" >&2
  find "$db_root" -mindepth 1 -maxdepth 1 -type d -exec rm -rf {} +
  fetch_config="$cargo_home/.advisory-fetch-config.toml"
  {
    printf '[advisories]\n'
    printf 'db-urls = ["%s"]\n' "$mirror_locator"
  } >"$fetch_config"
  (cd "$repo_root" && CARGO_HOME="$cargo_home" "$cargo_bin" deny --locked --config "$fetch_config" fetch db)
  printf '%s' "$mirror_locator" >"$locator_marker"
fi

repo_count=$(find "$db_root" -mindepth 1 -maxdepth 1 -type d | wc -l | tr -d '[:space:]')
if [ "$repo_count" -ne 1 ]; then
  echo "ERROR: expected exactly one mirror repository under $db_root after provisioning, found $repo_count; remove $cargo_home and retry." >&2
  exit 1
fi

echo "$cargo_home"
