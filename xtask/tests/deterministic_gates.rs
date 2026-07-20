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
    let mut make_resolving = 0;
    let mut make_nonresolving = 0;
    for (index, line) in makefile.lines().enumerate() {
        let Some(command) = makefile_cargo_subcommand(line) else {
            continue;
        };
        match command {
            "build" | "test" | "clippy" | "run" => {
                make_resolving += 1;
                assert!(
                    line.contains("--locked"),
                    "Makefile:{} resolving cargo {command} invocation must use --locked: {line}",
                    index + 1
                );
            }
            // Advisory database refresh does not resolve the project dependency graph.
            "deny" if line.contains(" deny fetch db") => {}
            "deny" => {
                make_resolving += 1;
                assert!(
                    line.contains("--locked"),
                    "Makefile:{} resolving cargo deny invocation must use --locked: {line}",
                    index + 1
                );
            }
            "fmt" | "clean" => {
                make_nonresolving += 1;
                assert!(
                    !line.contains("--locked"),
                    "Makefile:{} cargo {command} must not use --locked: {line}",
                    index + 1
                );
            }
            _ => {}
        }
    }
    assert!(
        make_resolving > 0,
        "Makefile has no resolving cargo invocations"
    );
    assert_eq!(
        make_nonresolving, 2,
        "Makefile must contain exactly the fmt and clean non-resolving cargo invocations"
    );

    for path in [
        "scripts/win-ci.cmd",
        "scripts/win-app-build.cmd",
        "scripts/win-package.cmd",
    ] {
        let text = read(&root, path);
        let mut invocations = 0;
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.starts_with("cargo ") {
                invocations += 1;
                assert!(
                    trimmed.contains("--locked"),
                    "{path}:{} executable cargo invocation must use --locked: {trimmed}",
                    index + 1
                );
            }
        }
        assert!(invocations > 0, "{path} has no executable cargo invocation");
    }

    let purity = read(&root, "xtask/src/purity.rs");
    assert!(purity.contains("std::env::var(\"CARGO\")"));
    assert!(purity.contains("Command::new(cargo)"));
    assert!(purity.contains(
        "\"tree\",\n                \"--locked\",\n                \"-p\",\n                package_name,\n                \"--target\",\n                \"all\",\n                \"--all-features\",\n                \"-e\",\n                \"normal,build,dev\",\n                \"--prefix\",\n                \"none\","
    ));
    assert!(
        purity.contains("\"metadata\", \"--locked\", \"--format-version\", \"1\", \"--no-deps\"")
    );

    let contract_test = read(&root, "xtask/tests/contract_not_stale.rs");
    assert!(contract_test.contains("\"run\", \"--locked\","));
}

#[test]
fn cargo_deny_and_transfer_preflights_are_mandatory() {
    let root = repo_root();
    let makefile = read(&root, "Makefile");
    let deny_toml = read(&root, "deny.toml");
    let deny_preflight = read(&root, "scripts/preflight-cargo-deny.sh");

    assert!(deny_preflight.contains("required=0.20.2"));
    assert!(deny_preflight.contains("cargo install cargo-deny --version $required --locked"));
    assert!(deny_preflight.contains("exit 1"));
    for advisory in ["RUSTSEC-2026-0194", "RUSTSEC-2026-0195"] {
        let line = deny_toml
            .lines()
            .find(|line| line.contains(advisory))
            .unwrap_or_else(|| panic!("deny.toml missing {advisory}"));
        assert!(
            line.contains("Owner: VPE."),
            "deny.toml {advisory} ignore must name Owner: VPE"
        );
    }
    assert!(makefile
        .lines()
        .any(|line| line == "ci: preflight-toolchain preflight-cargo-deny"));
    assert!(makefile
        .lines()
        .any(|line| line == "audit: preflight-toolchain preflight-cargo-deny"));
    assert!(makefile.contains(
        "ERROR: RustSec advisory database refresh failed; no current advisory result was produced."
    ));

    assert!(makefile.contains(
        "sync-win-host: require-win-remote-host\n\t@WIN_REMOTE_HOST=\"$(WIN_REMOTE_HOST)\" GIT=\"$(GIT)\" SCP=\"$(SCP)\" sh scripts/sync-win-host.sh"
    ));
    let sync_script = read(&root, "scripts/sync-win-host.sh");
    let guard = sync_script.find("phase=guard").expect("tree guard phase");
    let bundle = sync_script
        .find("phase=create-bundle")
        .expect("bundle creation phase");
    let scp = sync_script.find("phase=scp").expect("SCP transfer phase");
    assert!(
        guard < bundle && bundle < scp,
        "tree guard, bundle creation, and SCP phases must stay ordered"
    );
    assert!(sync_script[guard..bundle].contains("check-win-sync-tree.sh"));
    assert!(sync_script[bundle..scp].contains("\"$GIT\" bundle create"));

    for path in [
        "scripts/preflight-toolchain.sh",
        "scripts/preflight-cargo-deny.sh",
        "scripts/check-win-sync-tree.sh",
        "scripts/sync-win-host.sh",
        "scripts/win-host-ci.sh",
        "scripts/preflight-toolchain.cmd",
        "scripts/lib/preflight-toolchain.test.cmd",
        "scripts/win-ci.cmd",
        "scripts/win-app-build.cmd",
        "scripts/win-package.cmd",
    ] {
        let text = read(&root, path);
        let is_cmd = path.ends_with(".cmd");
        let failure_exit = if is_cmd { "exit /b 1" } else { "exit 1" };
        let mut has_failure_exit = false;
        for (index, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            if trimmed.is_empty() || script_comment(trimmed, is_cmd) {
                continue;
            }
            let lower = trimmed.to_ascii_lowercase();
            assert!(
                !lower.contains("skipping"),
                "{path}:{} contains a silent skipping path: {trimmed}",
                index + 1
            );
            has_failure_exit |= lower.contains(failure_exit);
        }
        assert!(
            has_failure_exit,
            "{path} must contain a nonzero failure exit ({failure_exit})"
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

fn makefile_cargo_subcommand(line: &str) -> Option<&str> {
    let trimmed = line.trim_start();
    if trimmed.starts_with('#') {
        return None;
    }
    let command_line = trimmed.strip_prefix('@').unwrap_or(trimmed).trim_start();
    if command_line.starts_with("echo ") {
        return None;
    }

    let mut tokens = command_line.split_whitespace();
    while let Some(token) = tokens.next() {
        if token == "$(CARGO)" || token == "cargo" {
            return tokens.next();
        }
    }
    None
}

fn script_comment(trimmed: &str, is_cmd: bool) -> bool {
    if is_cmd {
        let lower = trimmed.to_ascii_lowercase();
        lower.starts_with("::")
            || lower == "rem"
            || lower
                .strip_prefix("rem")
                .is_some_and(|rest| rest.starts_with(' ') || rest.starts_with('\t'))
    } else {
        trimmed.starts_with('#')
    }
}
