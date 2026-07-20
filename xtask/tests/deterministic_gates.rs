// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};

#[test]
fn rust_toolchain_pin_and_repair_verb_are_consistent() {
    let root = repo_root();
    let toolchain = read(&root, "rust-toolchain.toml");
    let cargo_toml = read(&root, "Cargo.toml");
    let makefile = read(&root, "Makefile");
    let shell_preflight = read(&root, "scripts/preflight-toolchain.sh");
    let cmd_preflight = read(&root, "scripts/preflight-toolchain.cmd");

    assert!(toolchain.lines().any(|line| line == "channel = \"1.96.0\""));
    assert!(cargo_toml
        .lines()
        .any(|line| line == "rust-version = \"1.96.0\""));
    assert!(makefile.lines().any(|line| line == "rust-toolchain:"));
    for required in [
        "rustup toolchain install \"$$version\"",
        "--profile minimal",
        "--component rustfmt",
        "--component clippy",
        "--target x86_64-pc-windows-msvc",
    ] {
        assert!(makefile.contains(required), "Makefile missing {required}");
    }
    for preflight in [&shell_preflight, &cmd_preflight] {
        assert!(preflight.contains("Rust toolchain mismatch"));
        assert!(preflight.contains("expected"));
        assert!(preflight.contains("actual"));
        assert!(preflight.contains("make rust-toolchain"));
    }
}

#[test]
fn every_gated_cargo_resolution_is_locked() {
    let root = repo_root();
    let makefile = read(&root, "Makefile");
    for required in [
        "$(CARGO) build --locked -p $(TAURI_BIN) --features custom-protocol",
        "$(CARGO) test --locked --workspace $(REMOTE_CRATES)",
        "$(CARGO) clippy --locked --workspace $(REMOTE_CRATES) --all-targets -- -D warnings",
        "$(CARGO) run --locked -q -p xtask -- contract --check",
        "$(CARGO) run --locked -q -p xtask -- purity-check",
        "$(CARGO) run --locked -q -p xtask -- contract",
        "$(CARGO) build --locked -p $(TAURI_BIN) --release --features custom-protocol",
        "$(CARGO) deny --offline --locked check bans licenses sources",
        "$(CARGO) deny --locked check advisories",
    ] {
        assert!(makefile.contains(required), "Makefile missing {required}");
    }
    assert_eq!(
        recipe_line(&makefile, "$(CARGO) fmt"),
        "\t$(CARGO) fmt --all --check"
    );
    assert_eq!(recipe_line(&makefile, "$(CARGO) clean"), "\t$(CARGO) clean");

    for (path, required) in [
        (
            "scripts/win-ci.cmd",
            &[
                "cargo build --locked --workspace --exclude solstone-windows-app",
                "cargo test --locked --workspace --exclude solstone-windows-app",
                "cargo run --locked -q -p xtask -- contract --check",
                "cargo run --locked -q -p xtask -- purity-check",
            ][..],
        ),
        (
            "scripts/win-app-build.cmd",
            &["cargo build --locked -p solstone-windows-app --features custom-protocol"][..],
        ),
        (
            "scripts/win-package.cmd",
            &["cargo build --locked -p solstone-windows-app --release --features custom-protocol"]
                [..],
        ),
    ] {
        let text = read(&root, path);
        for command in required {
            assert!(text.contains(command), "{path} missing {command}");
        }
    }

    let xtask = read(&root, "xtask/src/main.rs");
    assert!(xtask.contains("std::env::var(\"CARGO\")"));
    assert!(xtask.contains("Command::new(&cargo)"));
    assert!(xtask.contains("\"tree\",\n                \"--locked\","));

    let contract_test = read(&root, "xtask/tests/contract_not_stale.rs");
    assert!(contract_test.contains("\"run\", \"--locked\","));
}

#[test]
fn cargo_deny_and_transfer_preflights_are_mandatory() {
    let root = repo_root();
    let makefile = read(&root, "Makefile");
    let deny_preflight = read(&root, "scripts/preflight-cargo-deny.sh");

    assert!(deny_preflight.contains("required=0.20.2"));
    assert!(deny_preflight.contains("cargo install cargo-deny --version $required --locked"));
    assert!(deny_preflight.contains("exit 1"));
    assert!(makefile
        .lines()
        .any(|line| line == "ci: preflight-toolchain preflight-cargo-deny"));
    assert!(makefile
        .lines()
        .any(|line| line == "audit: preflight-toolchain preflight-cargo-deny"));
    assert!(makefile.contains(
        "ERROR: RustSec advisory database refresh failed; no current advisory result was produced."
    ));

    let sync_start = makefile
        .find("sync-win-host:")
        .expect("sync-win-host target");
    let sync_recipe = &makefile[sync_start..];
    let guard = sync_recipe
        .find("sh scripts/check-win-sync-tree.sh")
        .expect("tree guard");
    let bundle = sync_recipe
        .find("git bundle create")
        .expect("bundle creation");
    let scp = sync_recipe.find("$(WIN_SCP)").expect("SCP transfer");
    assert!(
        guard < bundle && guard < scp,
        "tree guard must run before remote work"
    );

    for path in [
        "scripts/preflight-toolchain.sh",
        "scripts/preflight-cargo-deny.sh",
        "scripts/check-win-sync-tree.sh",
        "scripts/preflight-toolchain.cmd",
        "scripts/win-ci.cmd",
        "scripts/win-app-build.cmd",
        "scripts/win-package.cmd",
    ] {
        let text = read(&root, path);
        assert!(
            !text.to_ascii_lowercase().contains("skipping"),
            "{path} contains a silent skipping path"
        );
    }
}

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn read(root: &Path, relative: &str) -> String {
    let path = root.join(relative);
    fs::read_to_string(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn recipe_line<'a>(makefile: &'a str, prefix: &str) -> &'a str {
    makefile
        .lines()
        .find(|line| line.trim_start().starts_with(prefix))
        .unwrap_or_else(|| panic!("Makefile recipe missing {prefix}"))
}
