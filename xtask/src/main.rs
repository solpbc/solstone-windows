// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Workspace task runner.
//!
//! Verbs:
//! - `contract` — regenerate `automation-contract.json` + the `ui/src/lib/contract.ts` codegen.
//! - `contract --check` — regenerate both in memory and exit 1 on drift (the `make ci` gate and the `contract_not_stale` test both invoke it).
//! - `purity-check` — fail if the `windows` family reaches any pure-tier crate, even target-gated (run by `make ci` and the remote mill gate `scripts/win-ci.cmd`).
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
        Some("purity-check") => cmd_purity_check(),
        Some("package") => cmd_package(),
        Some("dev") => cmd_dev(),
        _ => {
            eprintln!("usage: cargo xtask <contract [--check] | purity-check | package | dev>");
            ExitCode::from(2)
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

/// Pure-tier crates: must never depend on the `windows` family, even
/// target-gated. Keep in sync with the pure tier (AGENTS.md §Source Layout,
/// deny.toml). A new pure crate MUST be listed here or it goes unchecked.
const PURE_CRATES: &[&str] = &[
    "observer-model",
    "observer-segment",
    "observer-state",
    "observer-health",
    "observer-recovery",
    "observer-lifecycle",
    "observer-contract",
    "observer-exclusion",
    "observer-hotkey",
    "observer-mic",
    "observer-retention",
    "observer-update",
    "observer-pl",
    "observer-nv12",
];

/// Fail if the `windows` family reaches any pure-tier crate. `--target all` is
/// essential: the platform tier declares `windows` gated to cfg(windows), so a
/// leak is invisible to a host-target `cargo tree` on a non-Windows box.
fn cmd_purity_check() -> ExitCode {
    let root = repo_root();
    let mut violations = Vec::new();
    for pkg in PURE_CRATES {
        let out = std::process::Command::new("cargo")
            .args([
                "tree",
                "-p",
                pkg,
                "--target",
                "all",
                "-e",
                "normal,build",
                "--prefix",
                "none",
            ])
            .current_dir(&root)
            .output();
        let out = match out {
            Ok(o) => o,
            Err(e) => {
                eprintln!("purity-check: failed to run cargo tree for {pkg}: {e}");
                return ExitCode::FAILURE;
            }
        };
        if !out.status.success() {
            eprintln!(
                "purity-check: cargo tree failed for {pkg}:\n{}",
                String::from_utf8_lossy(&out.stderr)
            );
            return ExitCode::FAILURE;
        }
        for line in String::from_utf8_lossy(&out.stdout).lines() {
            if line.trim_start().starts_with("windows") {
                violations.push(format!("{pkg} -> {}", line.trim()));
            }
        }
    }
    if violations.is_empty() {
        println!(
            "purity-check: pure tier is windows-free ({} crates)",
            PURE_CRATES.len()
        );
        ExitCode::SUCCESS
    } else {
        eprintln!("purity-check: the windows family leaked into the pure tier:");
        for v in &violations {
            eprintln!("  {v}");
        }
        eprintln!("pure-tier crates must never depend on `windows` (even target-gated).");
        eprintln!("move the OS-bound code into a platform-tier crate (capture-*/platform-win).");
        ExitCode::FAILURE
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
