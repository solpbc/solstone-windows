// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Drift gate as a plain test.
//!
//! `cargo test` alone catches automation-contract drift: this shells the xtask
//! `contract --check` verb (the same gate `make ci` runs) and fails if the
//! committed `automation-contract.json` / `ui/src/lib/contract.ts` differ from
//! what the source of truth would generate. So a forgotten `make contract` after
//! editing the contract crate turns a normal `cargo test` red.

use std::process::Command;

#[test]
fn contract_not_stale() {
    let status = Command::new(env!("CARGO"))
        .args(["run", "--quiet", "-p", "xtask", "--", "contract", "--check"])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .status()
        .expect("failed to spawn `cargo run -p xtask -- contract --check`");

    assert!(
        status.success(),
        "automation contract is stale — run `make contract` and commit the result"
    );
}
