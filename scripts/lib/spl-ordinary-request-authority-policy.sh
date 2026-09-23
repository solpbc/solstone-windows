#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu

SCRIPT_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
DEFAULT_ROOT=$(CDPATH= cd -- "$SCRIPT_DIR/../.." && pwd)
ROOT=$DEFAULT_ROOT

if [ "${1:-}" = "--root" ]; then
  [ "$#" -ge 2 ] || { echo "spl-ordinary-request-authority-policy: --root requires a directory" >&2; exit 2; }
  ROOT=$2
  shift 2
fi

if ! ROOT=$(CDPATH= cd -- "$ROOT" 2>/dev/null && pwd); then
  echo "spl-ordinary-request-authority-policy: repository root is unavailable" >&2
  exit 2
fi

# The pairing authority policy owns the spl-rust revision and lock checks. Run
# it rather than copying a second pin into this policy.
sh "$SCRIPT_DIR/spl-pairing-authority-policy.sh" --root "$ROOT" >/dev/null

violations=0
violation() {
  echo "spl-ordinary-request-authority-policy: violation: $1" >&2
  violations=$((violations + 1))
}

ordinary="$ROOT/crates/pl-transport-win/src/ordinary_request.rs"
client="$ROOT/crates/pl-transport-win/src/client.rs"
[ -f "$ordinary" ] || violation "ordinary_request.rs is missing"
[ -f "$client" ] || violation "client.rs is missing"

if [ -f "$ordinary" ]; then
  for route in \
    ClientsSelfGet ClientsSelfPut RelayAccessGet IngestPost IngestManifestGet \
    IngestManifestDayGet IngestSegmentsDayGet SystemStatusGet; do
    grep -q "$route" "$ordinary" || violation "ordinary route $route is missing or unclassified"
  done
  grep -q "ReplayPolicy::ForbidAfterWrite" "$ordinary" || violation "clients-self PUT lacks explicit ForbidAfterWrite"
  grep -q "ReplayPolicy::ReplaySafe" "$ordinary" || violation "ordinary replay-safe routes are absent"
fi

if [ -f "$client" ]; then
  grep -q "TransportClient::new_with_publication" "$client" || violation "ordinary holder does not construct TransportClient with publication"
  grep -q "\.request(" "$client" || violation "ordinary holder does not call TransportClient::request"
  if grep -qE "async fn send\(|async fn send_over_relay\(" "$client"; then
    violation "client retains an obsolete ordinary send loop"
  fi
  if grep -qE "request_once(_observed|_observed_with_cap)?\(" "$client"; then
    violation "client routes ordinary work through local request_once"
  fi
  if grep -qE "crate::observe|copy_shared_observation" "$client"; then
    violation "client retains a local observer seam"
  fi
  if ! awk '
    /async fn ordinary_request/ { in_request = 1 }
    in_request && /\.request\(/ { found = 1 }
    END { exit !found }
  ' "$client"; then
    violation "ordinary request dispatcher does not reach TransportClient::request"
  fi
  for helper_route in \
    'get_clients_self:ClientsSelfGet' \
    'put_clients_self:ClientsSelfPut' \
    'get_relay_access:RelayAccessGet' \
    'ingest:IngestPost' \
    'ingest_manifest:IngestManifestGet' \
    'ingest_manifest_day:IngestManifestDayGet' \
    'list_segments:IngestSegmentsDayGet' \
    'system_status:SystemStatusGet'; do
    helper=${helper_route%%:*}
    route=${helper_route#*:}
    grep -q "async fn $helper" "$client" || violation "ordinary helper $helper is missing"
    grep -q "OrdinaryRequest::$route" "$client" || violation "ordinary helper $helper lacks table route $route"
  done
fi

# These modules define production ordinary entry points. The bridge's carrier is
# intentionally excluded; it may use only dial_carrier through its own module.
for relative in src/client.rs src/post_connect.rs src/coordinator.rs src/journal_version.rs src/integration/ops.rs; do
  file="$ROOT/crates/pl-transport-win/$relative"
  [ -f "$file" ] || continue
  if grep -qE "(ObserverClient::send|send_over_relay\(|request_once(_observed|_observed_with_cap|_relay)?\(|MuxCarrier)" "$file"; then
    violation "$relative contains an ordinary-request bypass"
  fi
  if grep -qE "(crate::observe|copy_shared_observation)" "$file"; then
    violation "$relative retains a local observer algorithm"
  fi
done

for relative in src/journal_bridge.rs; do
  file="$ROOT/crates/pl-transport-win/$relative"
  [ -f "$file" ] || violation "journal bridge allowlisted file $relative is missing"
  if [ -f "$file" ]; then
    for path in \
      '/app/network/api/clients/self' \
      '/app/network/api/relay/access' \
      '/app/devices/ingest' \
      '/app/devices/ingest/manifest' \
      '/app/devices/ingest/segments' \
      '/api/system/status'; do
      if grep -F -q "$path" "$file"; then
        violation "$relative contains an ordinary request route $path"
      fi
    done
  fi
done

for src_file in $(find "$ROOT/crates/pl-transport-win/src" -type f -name '*.rs'); do
  case "$src_file" in
    */client.rs|*/ordinary_request.rs) continue ;;
    *)
      if grep -qE "ingest_manifest|IngestManifestGet|IngestManifestDayGet" "$src_file"; then
        rel=${src_file#"$ROOT/crates/pl-transport-win/"}
        violation "$rel contains obsolete manifest route reference"
      fi
      ;;
  esac
done

if [ "$violations" -ne 0 ]; then
  echo "spl-ordinary-request-authority-policy: $violations policy violation(s) found" >&2
  exit 1
fi

echo "spl-ordinary-request-authority-policy: ordinary request authority checks passed"
