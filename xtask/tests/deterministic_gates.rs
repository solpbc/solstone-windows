// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

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
            if trimmed.starts_with("cargo ") || trimmed.starts_with("call \"%SELECTED_CARGO%\"") {
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
        "\"tree\",\n                \"--locked\",\n                \"-p\",\n                package_name,\n                \"--target\",\n                \"all\",\n                \"--all-features\",\n                \"-e\",\n                \"normal,build,dev\",\n                \"--prefix\",\n                \"depth\",\n                \"--no-dedupe\","
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
    assert!(deny_preflight.contains("Run 'make provision-cargo-deny'."));
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

#[test]
fn dependency_and_release_lockdown_topology_is_static() {
    let root = repo_root();
    let makefile = read(&root, "Makefile");
    let gitignore = read(&root, ".gitignore");

    assert!(root.join("ui/package-lock.json").is_file());
    assert!(!gitignore
        .lines()
        .any(|line| line == "/ui/package-lock.json"));

    assert!(makefile.contains("install:\n\t@if [ -f ui/package.json ]; then npm --prefix ui ci;"));
    assert!(makefile.contains("ui-deps-update:\n\tnpm --prefix ui install"));
    assert!(makefile.contains("build: preflight-toolchain\n\tnpm --prefix ui ci --offline"));
    assert!(makefile.contains("ui-test:\n\tnpm --prefix ui ci --offline"));
    assert!(makefile.contains("\"$$npm_path\" --prefix ui ci --offline"));
    for path in ["scripts/win-app-build.cmd", "scripts/win-package.cmd"] {
        assert!(read(&root, path).contains("--prefix ui ci --offline"));
    }

    let executable_files = [
        "Makefile",
        "scripts/win-app-build.cmd",
        "scripts/win-package.cmd",
        "src-tauri/tauri.conf.json",
    ];
    let mut npm_installs = Vec::new();
    for path in executable_files {
        for (index, line) in read(&root, path).lines().enumerate() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with('#') && trimmed.contains("npm --prefix ui install") {
                npm_installs.push(format!("{path}:{}:{trimmed}", index + 1));
            }
        }
    }
    assert_eq!(
        npm_installs.len(),
        1,
        "only ui-deps-update may execute npm install: {npm_installs:?}"
    );
    assert!(npm_installs[0].contains("Makefile"));

    assert!(makefile
        .contains("provision-cargo-deny:\n\tcargo install cargo-deny --version 0.20.2 --locked"));
    for target in ["build:", "test:", "package:", "ci:", "audit:"] {
        let line = makefile
            .lines()
            .find(|line| line.starts_with(target))
            .expect("required make target");
        assert!(!line.contains("provision-cargo-deny"));
    }

    let contract: Value = serde_json::from_str(&read(&root, "packaging/release-toolchain.json"))
        .expect("release toolchain JSON");
    assert_eq!(
        contract["tools"]["dotnet"]["expected"]["version"],
        "8.0.422"
    );
    assert_eq!(contract["tools"]["vpk"]["expected"]["packageId"], "vpk");
    assert_eq!(contract["tools"]["vpk"]["expected"]["version"], "1.2.0");
    assert_eq!(
        contract["tools"]["msvc-cl"]["expected"]["compilerVersion"],
        "19.44.35228"
    );
    assert_eq!(
        contract["tools"]["msvc-cl"]["expected"]["toolsetVersion"],
        "14.44.35207"
    );
}

#[test]
fn publication_and_parallel_version_sources_are_locked_out() {
    let root = repo_root();
    let exact_message = "ERROR: publication locked: direct publication is disabled; release publication belongs to the aggregate provenance publisher.";
    let guard = read(&root, "scripts/lib/publication-guard.sh");
    assert!(guard.contains(exact_message));

    for path in [
        "scripts/publish-gh.sh",
        "scripts/publish-r2.sh",
        "scripts/publish-winget.sh",
        "scripts/publish-scoop.sh",
    ] {
        let script = read(&root, path);
        assert!(script.contains("lib/publication-guard.sh"));
        assert!(script.contains("publication_guard"));
        for forbidden in [" gh ", "wrangler", "curl", "jq", "scp", "VERSION="] {
            assert!(!script.contains(forbidden), "{path} retains {forbidden}");
        }
    }
    assert!(!root.join("scripts/lib/artifact-names.sh").exists());
    assert!(!root.join("scripts/lib/artifact-names.test.sh").exists());

    let channels = read(&root, "scripts/check-channels.sh");
    assert!(channels.contains("[ \"$#\" -eq 0 ]"));
    assert!(channels.contains("cargo run --locked -q -p xtask -- version-gate"));
    assert!(!channels.contains("grep -m1 '^version = ' Cargo.toml"));
    assert!(!channels.contains("make publish-winget"));
    assert!(!channels.contains("make publish-scoop"));

    let package = read(&root, "scripts/package.ps1");
    assert!(!package.contains("--dump-state"));
    assert!(!package.contains("[string]$Version"));
    assert!(!package.to_ascii_lowercase().contains("signtool verify"));
    assert!(package.contains("$Selection.tools.vpk.path"));
    assert!(package.contains("$Selection.tools.smctl.path"));
    assert!(package.contains("-SmctlPath $SmctlPath"));

    let win_ci = read(&root, "scripts/win-ci.cmd");
    for test in [
        "preflight-release-tools.test.ps1",
        "lock-guard.test.ps1",
        "package-entrypoints.test.ps1",
    ] {
        assert!(win_ci.contains(test));
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
