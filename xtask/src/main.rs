// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Workspace task runner.
//!
//! Verbs:
//! - `contract` — regenerate `automation-contract.json` + the `ui/src/lib/contract.ts` codegen.
//! - `contract --check` — regenerate both in memory and exit 1 on drift (the `make ci` gate and the `contract_not_stale` test both invoke it).
//! - `observer-contract check` — verify the vendored observer-client authority bundle and adoption record.
//! - `rust-release-manifest check` — offline schema, semantic, ledger, and bundle verification.
//! - `purity-check` — fail if the `windows` family reaches any strict workspace member's shipped graph (every member except the reviewed Windows-capable exception set), even target-gated; members come from `cargo metadata`, with normal/build traversal under `--target all --all-features`.
//! - `version-gate [--root <path>]` — resolve the product version from cargo metadata and verify every committed release version surface.
//! - `package` — Velopack packaging (delegates to the Windows script; a stub off the build box).
//! - `dev` — developer convenience launcher (stub).

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use observer_contract::generate_contract;

/// The committed contract artifact's filename at the repo root.
const CONTRACT_FILE: &str = "automation-contract.json";
/// The generated webview binding.
const UI_CONTRACT_TS: &str = "ui/src/lib/contract.ts";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("contract") => {
            let check = args.iter().any(|a| a == "--check");
            cmd_contract(check)
        }
        Some("observer-contract")
            if args.get(1).map(String::as_str) == Some("check") && args.len() == 2 =>
        {
            cmd_observer_contract_check()
        }
        Some("rust-release-manifest")
            if args.get(1).map(String::as_str) == Some("check") && args.len() == 2 =>
        {
            cmd_rust_release_manifest_check()
        }
        Some("purity-check") => cmd_purity_check(),
        Some("version-gate") => cmd_version_gate(&args),
        Some("package") => cmd_package(),
        Some("dev") => cmd_dev(),
        _ => {
            eprintln!(
                "usage: cargo xtask <contract [--check] | observer-contract check | rust-release-manifest check | purity-check | version-gate [--root <path>] | package | dev>\n  contract [--check]: generate or verify the AutomationId/state-token contract\n  observer-contract check: verify the vendored observer-client authority bundle\n  rust-release-manifest check: offline manifest and current-bundle verification selected by MANIFEST or RELEASE_DIR\n  version-gate [--root <path>]: verify every committed release version surface"
            );
            ExitCode::from(2)
        }
    }
}

fn cmd_rust_release_manifest_check() -> ExitCode {
    let root = repo_root();
    let cargo = xtask::version_gate::configured_cargo();
    let git = std::env::var_os("GIT").unwrap_or_else(|| "git".into());
    let manifest = std::env::var_os("MANIFEST").filter(|value| !value.is_empty());
    let release_dir = std::env::var_os("RELEASE_DIR").filter(|value| !value.is_empty());
    match xtask::rust_release_manifest::run_check(
        &root,
        &cargo,
        &git,
        manifest.as_deref(),
        release_dir.as_deref(),
    ) {
        Ok(report) => {
            match report.mode {
                xtask::rust_release_manifest::ClassificationMode::FixtureSelfCheck => println!(
                    "rust release manifest: offline fixtures and deterministic rendering verified"
                ),
                xtask::rust_release_manifest::ClassificationMode::SiblingBytesOnly => {
                    println!("rust release manifest: named sibling bytes verified");
                }
                xtask::rust_release_manifest::ClassificationMode::CompleteCurrentBundle => {
                    println!("rust release manifest: complete current bundle verified");
                }
            }
            if let Some(disclaimer) = report.disclaimer {
                println!("{disclaimer}");
            }
            ExitCode::SUCCESS
        }
        Err(xtask::rust_release_manifest::ManifestError::Usage) => {
            eprintln!(
                "usage: set at most one of MANIFEST=<path> or RELEASE_DIR=<path> for rust-release-manifest check"
            );
            ExitCode::from(2)
        }
        Err(error) => {
            eprintln!("rust release manifest check failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_version_gate(args: &[String]) -> ExitCode {
    let root = match args {
        [_] => repo_root(),
        [_, flag, path] if flag == "--root" && !path.is_empty() => PathBuf::from(path),
        _ => {
            eprintln!("usage: cargo xtask version-gate [--root <path>]");
            return ExitCode::from(2);
        }
    };
    let cargo = xtask::version_gate::configured_cargo();
    match xtask::version_gate::run(&root, &cargo) {
        Ok(version) => {
            println!("{version}");
            ExitCode::SUCCESS
        }
        Err(xtask::version_gate::VersionGateError::Authority(error)) => {
            eprintln!("ERROR: version-gate: {error}");
            ExitCode::FAILURE
        }
        Err(xtask::version_gate::VersionGateError::Surface(mismatches)) => {
            for mismatch in mismatches {
                eprintln!("{}", mismatch.diagnostic());
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_observer_contract_check() -> ExitCode {
    let root = repo_root();
    let consumer_dir = root.join("contracts/observer-client");
    match xtask::observer_contract::verify(
        &consumer_dir.join("bundle"),
        &consumer_dir.join("adoption.json"),
    ) {
        Ok(report) => {
            println!(
                "observer-client authority bundle: OK; local offline structural evidence verified for version {} ({} operations, {} fixtures, {} vectors)",
                report.bundle_semver,
                report.operation_count,
                report.fixture_count,
                report.vector_count
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("observer-client authority bundle check failed: {error}");
            ExitCode::FAILURE
        }
    }
}

/// Repo root = the workspace dir (xtask manifest's parent).
fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask lives one level below the repo root")
        .to_path_buf()
}

fn cmd_contract(check: bool) -> ExitCode {
    let root = repo_root();
    let json = generate_contract();
    let ts = generate_ts_binding(&json);

    let json_path = root.join(CONTRACT_FILE);
    let ts_path = root.join(UI_CONTRACT_TS);

    if check {
        let mut drift = false;
        drift |= report_drift(&json_path, &json);
        drift |= report_drift(&ts_path, &ts);
        if drift {
            eprintln!("contract drift detected — run `make contract` and commit the result");
            return ExitCode::FAILURE;
        }
        println!("contract up to date");
        ExitCode::SUCCESS
    } else {
        if let Some(parent) = ts_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&json_path, &json) {
            eprintln!("failed to write {}: {e}", json_path.display());
            return ExitCode::FAILURE;
        }
        if let Err(e) = std::fs::write(&ts_path, &ts) {
            eprintln!("failed to write {}: {e}", ts_path.display());
            return ExitCode::FAILURE;
        }
        println!("wrote {} and {}", json_path.display(), ts_path.display());
        ExitCode::SUCCESS
    }
}

/// Compare on-disk content to freshly generated; print a hint and return true on
/// mismatch (or missing file).
fn report_drift(path: &Path, fresh: &str) -> bool {
    match std::fs::read_to_string(path) {
        Ok(existing) if existing == fresh => false,
        Ok(_) => {
            eprintln!("stale: {}", path.display());
            true
        }
        Err(_) => {
            eprintln!("missing: {}", path.display());
            true
        }
    }
}

/// The `ui/src/lib/contract.ts` binding, generated from the same JSON the harness
/// reads. Deterministic; carries the DO-NOT-EDIT marker and an SPDX header (it is
/// generated *source*, so the header convention applies).
fn generate_ts_binding(contract_json: &str) -> String {
    let mut out = String::new();
    out.push_str("// SPDX-License-Identifier: AGPL-3.0-only\n");
    out.push_str("// Copyright (c) 2026 sol pbc\n");
    out.push_str("//\n");
    out.push_str("// GENERATED — DO NOT EDIT. Run `make contract`.\n");
    out.push_str(
        "// Source of truth: the observer-contract crate -> automation-contract.json.\n\n",
    );
    out.push_str("export const automationContract = ");
    // Embed the canonical JSON verbatim so the TS and JSON can never disagree.
    out.push_str(contract_json.trim_end());
    out.push_str(" as const;\n\n");
    out.push_str("export type AutomationContract = typeof automationContract;\n");
    out
}

fn cmd_purity_check() -> ExitCode {
    let root = repo_root();
    let cargo = xtask::purity::configured_cargo();
    match xtask::purity::run_purity_check(&root, std::ffi::OsStr::new(&cargo)) {
        Ok(witness) => {
            println!(
                "purity-check: inspected {} workspace members and {} dependency edges; {} strict members are windows-free; {} validated exceptions reach the windows family",
                witness.member_count,
                witness.inspected_edge_count,
                witness.strict_count,
                witness.exception_count
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_package() -> ExitCode {
    eprintln!("xtask package: delegate to scripts/package.ps1 on the Windows build box (not yet implemented here)");
    ExitCode::SUCCESS
}

fn cmd_dev() -> ExitCode {
    eprintln!("xtask dev: developer launcher (not yet implemented)");
    ExitCode::SUCCESS
}
