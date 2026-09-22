<!-- SPDX-License-Identifier: AGPL-3.0-only -->
<!-- Copyright (c) 2026 sol pbc -->

# SPL Pair-Link Definition Bundle

This directory vendors the published SPL pair-link definition bundle (`bundle_semver: 8.0.0`) as immutable, test-only bytes.

- `bundle/`: Immutable authority files. The five JSON documents in this directory must remain byte-identical to their authority pins.
- `adoption.json`: Consumer adoption metadata record matching the sibling SPL adoption schema.
- `coverage-gaps.md`: Engineering-internal report of predicate coverage gaps and unmapped semantic differences.
