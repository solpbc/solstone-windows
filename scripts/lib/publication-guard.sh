#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

publication_guard() {
  echo "ERROR: publication locked: direct publication is disabled; release publication belongs to the aggregate provenance publisher." >&2
  return 1
}
