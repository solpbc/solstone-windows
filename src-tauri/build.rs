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

    // Tauri dispatches IPC on the main thread: an async command's whole future is
    // built and moved there before the runtime polls it. At the MSVC default 1 MiB
    // reserve, pressing pair overflowed that thread ("thread 'main' has overflowed
    // its stack") and closed the app. Reserve 8 MiB. Windows commits stack pages
    // lazily, so the reserve costs address space, not memory. The release finalizer
    // refuses a build whose executable reserves less (`MAIN_THREAD_STACK_RESERVE`).
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc") {
        println!("cargo:rustc-link-arg-bin=solstone-windows-app=/STACK:8388608");
    }

    tauri_build::build();
}
