// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

fn main() {
    // Optional source-commit stamp for the integration mode's artifact identity.
    // Forwarded only when the environment actually supplies it: a build without
    // it reports a null commit rather than a fabricated one, and a source-tarball
    // build (no `.git`) still succeeds — which is why this reads an environment
    // variable instead of shelling out to `git rev-parse`. The value's *shape* is
    // validated at runtime by `pl_transport_win::integration::validate_source_commit`,
    // so the build never has to decide what counts as a commit.
    println!("cargo:rerun-if-env-changed=SOLSTONE_SOURCE_COMMIT");
    if let Ok(commit) = std::env::var("SOLSTONE_SOURCE_COMMIT") {
        println!("cargo:rustc-env=SOLSTONE_SOURCE_COMMIT={commit}");
    }

    tauri_build::build();
}
