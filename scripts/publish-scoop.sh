#!/usr/bin/env sh
# SPDX-License-Identifier: AGPL-3.0-only
# Copyright (c) 2026 sol pbc

set -eu
. "$(dirname "$0")/lib/publication-guard.sh"
publication_guard
exit 1
