// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Workspace task runner.
//!
//! Verbs:
//! - `contract` — regenerate `automation-contract.json` + the `ui/src/lib/contract.ts` codegen.
//! - `contract --check` — regenerate both in memory and exit 1 on drift (the `make ci` gate and the `contract_not_stale` test both invoke it).
//! - `observer-contract check` — verify the vendored observer-client authority bundle and adoption record.
//! - `rust-release-manifest check` — offline schema, semantic, ledger, and bundle verification.
//! - `rust-release-manifest advisory-config` — materialize the deterministic isolated advisory policy.
//! - `rust-release-manifest prove-native` — install and smoke one exact signed finalized candidate.
//! - `purity-check` — fail if the `windows` family reaches any strict workspace member's shipped graph (every member except the reviewed Windows-capable exception set), even target-gated; members come from `cargo metadata`, with normal/build traversal under `--target all --all-features`.
//! - `version-gate [--root <path>]` — resolve the product version from cargo metadata and verify every committed release version surface.
//! - `dev` — developer convenience launcher (stub).

use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use observer_contract::generate_contract;
use xtask::release_exec::{resolve_path_program, CommandRunner};

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
        Some("advisory-audit") if args.len() == 1 => cmd_advisory_audit(),
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
        Some("rust-release-manifest")
            if args.get(1).map(String::as_str) == Some("advisory-config") =>
        {
            cmd_rust_release_manifest_advisory_config(&args)
        }
        Some("rust-release-manifest") if args.get(1).map(String::as_str) == Some("finalize") => {
            cmd_rust_release_manifest_finalize(&args)
        }
        Some("rust-release-manifest")
            if args.get(1).map(String::as_str) == Some("prove-native") =>
        {
            cmd_rust_release_manifest_prove_native(&args)
        }
        Some("transparency") if args.get(1).map(String::as_str) == Some("publish") => {
            cmd_transparency_publish(&args)
        }
        Some("transparency")
            if args.get(1).map(String::as_str) == Some("resign-pointer") && args.len() == 2 =>
        {
            cmd_transparency_resign_pointer()
        }
        Some("purity-check") => cmd_purity_check(),
        Some("version-gate") => cmd_version_gate(&args),
        Some("dev") => cmd_dev(),
        _ => {
            eprintln!(
                "usage: cargo xtask <advisory-audit | contract [--check] | observer-contract check | rust-release-manifest <check | advisory-config --db-root <isolated-absolute-path> --out <path> | finalize --expected-release-commit <40hex> [--sign] [--delta-base-full <basename> ...] | prove-native --release-dir <candidate>> | transparency <publish --release-dir <candidate> | resign-pointer> | purity-check | version-gate [--root <path>] | dev>\n  advisory-audit: verify the signed advisory packet and bundle, then check the locked graph offline\n  contract [--check]: generate or verify the AutomationId/state-token contract\n  observer-contract check: verify the vendored observer-client authority bundle\n  rust-release-manifest check: offline manifest and current-bundle verification selected by MANIFEST or RELEASE_DIR\n  rust-release-manifest advisory-config: materialize the deterministic isolated advisory policy\n  rust-release-manifest finalize: source-bound build-to-finalize transaction; selection JSON is read from stdin\n  rust-release-manifest prove-native: install and smoke one exact signed finalized candidate\n  transparency publish: archive and publish evidence for one validated candidate\n  transparency resign-pointer: refresh the signed latest pointer without a candidate\n  version-gate [--root <path>]: verify every committed release version surface"
            );
            ExitCode::from(2)
        }
    }
}

fn cmd_advisory_audit() -> ExitCode {
    let required = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let (Some(locator), Some(receipt), Some(public_key), Some(bundle)) = (
        required("SOLSTONE_ADVISORY_MIRROR_LOCATOR"),
        required("SOLSTONE_ADVISORY_RECEIPT"),
        required("SOLSTONE_ADVISORY_MIRROR_PUB"),
        required("SOLSTONE_ADVISORY_BUNDLE"),
    ) else {
        eprintln!(
            "advisory audit input gate failed; set all four required SOLSTONE_ADVISORY_* values and retry"
        );
        return ExitCode::FAILURE;
    };
    let receipt = PathBuf::from(receipt);
    let public_key = PathBuf::from(public_key);
    let bundle = PathBuf::from(bundle);
    if let Err(error) = xtask::advisory_audit::validate_advisory_audit_inputs(
        &locator,
        &receipt,
        &public_key,
        &bundle,
    ) {
        eprintln!("{error}");
        return ExitCode::FAILURE;
    }
    let programs = match xtask::advisory_audit::resolve_advisory_audit_programs() {
        Ok(programs) => programs,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let root = repo_root();
    let request = xtask::advisory_audit::AdvisoryAuditRequest {
        checkout_root: &root,
        locator: &locator,
        receipt_path: &receipt,
        public_key_path: &public_key,
        bundle_path: &bundle,
        programs: xtask::advisory_audit::AdvisoryAuditPrograms {
            git: &programs.git,
            minisign: &programs.minisign,
            cargo_deny: &programs.cargo_deny,
        },
        trust: xtask::advisory_audit::AdvisoryAuditTrust {
            public_key_sha256: xtask::release_advisory::MIRROR_MINISIGN_PUBKEY_SHA256,
            public_key_id: xtask::release_advisory::MIRROR_MINISIGN_PUBKEY_ID,
        },
    };
    let runner = xtask::release_exec::RemovedEnvironmentProcessCommandRunner::new(
        &xtask::advisory_audit::ADVISORY_AUDIT_REMOVED_ENV,
    );
    match xtask::advisory_audit::run_advisory_audit(
        &request,
        &runner,
        &xtask::release_clock::SystemClock,
    ) {
        Ok(witness) => {
            if std::io::stdout().write_all(&witness).is_err() {
                eprintln!("advisory audit output gate failed; restore stdout and retry");
                return ExitCode::FAILURE;
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_transparency_publish(args: &[String]) -> ExitCode {
    let release_dir = match args {
        [_, command, flag, value]
            if command == "publish" && flag == "--release-dir" && !value.is_empty() =>
        {
            PathBuf::from(value)
        }
        _ => return transparency_publish_usage(),
    };
    let Some(context) = transparency_command_context() else {
        return ExitCode::FAILURE;
    };
    let runner = xtask::release_exec::RemovedEnvironmentProcessCommandRunner::new(
        &xtask::transparency_publisher::TRANSPARENCY_ENV_NAMES,
    );
    if transparency_head_log_dirty(&context.root, &context.git_program, &runner) == Some(true) {
        let row = transparency_last_head_row(&context.root);
        eprintln!("{}", transparency_head_log_guard_diagnostic(&row));
        return ExitCode::FAILURE;
    }
    let release_dir = if release_dir.is_absolute() {
        release_dir
    } else {
        context.root.join(release_dir)
    };
    let cargo = xtask::version_gate::configured_cargo();
    let Some(cargo_program) = resolve_configured_program(&cargo) else {
        eprintln!(
            "terminal transparency candidate: observed unavailable Cargo program, expected an absolute or path-resolved executable; restore Cargo and retry"
        );
        return ExitCode::FAILURE;
    };
    let facts = match xtask::rust_release_manifest::gather_checkout_facts_with_runner(
        &context.root,
        &cargo_program,
        &context.git_program,
        &runner,
    ) {
        Ok(facts) => facts,
        Err(xtask::rust_release_manifest::ManifestError::SourceDirty { commit }) => {
            eprintln!("{}", transparency_dirty_source_diagnostic(&commit));
            return ExitCode::FAILURE;
        }
        Err(_) => {
            eprintln!(
                "terminal transparency candidate: observed unavailable checkout facts, expected a clean source-bound candidate; restore the checkout and retry"
            );
            return ExitCode::FAILURE;
        }
    };
    let evidence_dir = context
        .root
        .join(xtask::release_receipt::EVIDENCE_ROOT)
        .join(&facts.version);
    let transport = match xtask::transparency_transport::CurlTransparencyTransport::new(
        &runner,
        context.curl_program.clone(),
        context
            .root
            .join("target/release-transparency-curl-scratch"),
        context.environment.s3_endpoint.clone(),
        context.environment.base_url.clone(),
        context.environment.bucket.clone(),
        xtask::transparency_transport::TransparencyS3Credentials::new(
            context.environment.s3_access_key_id.clone(),
            context.environment.s3_secret_access_key.clone(),
        ),
    ) {
        Ok(transport) => transport,
        Err(_) => {
            eprintln!(
                "terminal transparency transport: observed invalid operator configuration, expected absolute tooling and HTTPS surfaces; correct the environment and retry"
            );
            return ExitCode::FAILURE;
        }
    };
    let request = xtask::transparency_publisher::TransparencyPublishRequest {
        checkout_root: &context.root,
        release_dir: &release_dir,
        evidence_dir: &evidence_dir,
        checkout_facts: &facts,
        environment: &context.environment,
        minisign_program: &context.minisign_program,
        curl_program: &context.curl_program,
    };
    match xtask::transparency_publisher::publish_transparency(
        &request,
        &transport,
        &runner,
        &xtask::release_clock::SystemClock,
    ) {
        Ok(result) => {
            println!(
                "transparency publication: product={} version={} seq={} entry_sha256={} archive_sha256={} elapsed_ms={}{}",
                result.product,
                result.version,
                result.seq,
                result.entry_sha256,
                result.archive_sha256.as_deref().unwrap_or("unavailable"),
                result.elapsed_ms,
                if result.already_published { " already-published" } else { "" }
            );
            println!(
                "public entry: {}/releases/{}/v/{}/ledger-entry.json",
                context.environment.base_url.trim_end_matches('/'),
                result.product,
                result.version
            );
            println!(
                "public ledger: {}/releases/{}/ledger.jsonl",
                context.environment.base_url.trim_end_matches('/'),
                result.product
            );
            println!(
                "public latest: {}/releases/{}/latest.json",
                context.environment.base_url.trim_end_matches('/'),
                result.product
            );
            println!(
                "{}",
                transparency_head_log_summary(transparency_head_log_dirty(
                    &context.root,
                    &context.git_program,
                    &runner,
                ))
            );
            if result.pointer_requires_resign {
                println!(
                    "transparency pointer: staged bytes were published after expiry; run make resign-transparency-pointer"
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

fn transparency_publish_usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask transparency publish --release-dir <target/release-candidate/VERSION>"
    );
    ExitCode::from(2)
}

fn cmd_transparency_resign_pointer() -> ExitCode {
    let Some(context) = transparency_command_context() else {
        return ExitCode::FAILURE;
    };
    let runner = xtask::release_exec::RemovedEnvironmentProcessCommandRunner::new(
        &xtask::transparency_publisher::TRANSPARENCY_ENV_NAMES,
    );
    let transport = match xtask::transparency_transport::CurlTransparencyTransport::new(
        &runner,
        context.curl_program.clone(),
        context
            .root
            .join("target/release-transparency-curl-scratch"),
        context.environment.s3_endpoint.clone(),
        context.environment.base_url.clone(),
        context.environment.bucket.clone(),
        xtask::transparency_transport::TransparencyS3Credentials::new(
            context.environment.s3_access_key_id.clone(),
            context.environment.s3_secret_access_key.clone(),
        ),
    ) {
        Ok(transport) => transport,
        Err(_) => {
            eprintln!(
                "terminal transparency transport: observed invalid operator configuration, expected absolute tooling and HTTPS surfaces; correct the environment and retry"
            );
            return ExitCode::FAILURE;
        }
    };
    let request = xtask::transparency_publisher::TransparencyResignRequest {
        checkout_root: &context.root,
        environment: &context.environment,
        minisign_program: &context.minisign_program,
        curl_program: &context.curl_program,
    };
    match xtask::transparency_publisher::resign_transparency_pointer(
        &request,
        &transport,
        &runner,
        &xtask::release_clock::SystemClock,
    ) {
        Ok(result) => {
            if let Some(discarded) = &result.discarded_recovery {
                println!(
                    "transparency pointer recovery: discarded stale record version={} chain_length={} tip_sha256={}",
                    discarded.version, discarded.chain_length, discarded.tip_sha256
                );
            }
            println!(
                "transparency pointer re-signed: product={} version={} chain_length={} tip_sha256={} valid_until={}",
                result.product,
                result.version,
                result.chain_length,
                result.tip_sha256,
                result.valid_until
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

struct TransparencyCommandContext {
    root: PathBuf,
    git_program: PathBuf,
    minisign_program: PathBuf,
    curl_program: PathBuf,
    environment: xtask::transparency_publisher::TransparencyEnvironment,
}

fn transparency_command_context() -> Option<TransparencyCommandContext> {
    let environment =
        match xtask::transparency_publisher::resolve_transparency_environment_with(|name| {
            std::env::var(name).ok()
        }) {
            Ok(environment) => environment,
            Err(error) => {
                eprintln!("{error}");
                return None;
            }
        };
    let minisign_program = match resolve_path_program("minisign") {
        Some(program) => program,
        None => {
            eprintln!(
                "terminal transparency preflight: observed minisign missing, expected minisign 0.11 or 0.12; install with cargo install minisign --locked and retry"
            );
            return None;
        }
    };
    let curl_program = match resolve_path_program("curl") {
        Some(program) => program,
        None => {
            eprintln!(
                "terminal transparency preflight: observed curl missing, expected curl with AWS SigV4 support; install curl and retry"
            );
            return None;
        }
    };
    let git_program = match resolve_path_program("git") {
        Some(program) => program,
        None => {
            eprintln!(
                "terminal transparency preflight: observed git missing, expected a local absolute Git executable; install git and retry"
            );
            return None;
        }
    };
    Some(TransparencyCommandContext {
        root: repo_root(),
        git_program,
        minisign_program,
        curl_program,
        environment,
    })
}

fn resolve_configured_program(program: &std::ffi::OsStr) -> Option<PathBuf> {
    let path = Path::new(program);
    if path.is_absolute() {
        if !path.is_file() {
            return None;
        }
        return std::fs::canonicalize(path).ok();
    }
    if path.components().count() != 1 {
        return None;
    }
    resolve_path_program(program.to_str()?)
}

fn transparency_head_log_dirty<R: CommandRunner + ?Sized>(
    root: &Path,
    git_program: &Path,
    runner: &R,
) -> Option<bool> {
    let root = root.to_str()?.to_owned();
    for cached in [false, true] {
        let mut args = vec!["-C".to_owned(), root.clone(), "diff".to_owned()];
        if cached {
            args.push("--cached".to_owned());
        }
        args.extend([
            "--quiet".to_owned(),
            "--".to_owned(),
            "transparency-head-log.jsonl".to_owned(),
        ]);
        match runner.run(git_program, &args, None, None).ok()?.status {
            0 => {}
            1 => return Some(true),
            _ => return None,
        }
    }
    Some(false)
}

fn transparency_head_log_summary(dirty: Option<bool>) -> &'static str {
    match dirty {
        Some(false) => "transparency witness: row present and committed",
        Some(true) => {
            "transparency witness: row written uncommitted; run git add transparency-head-log.jsonl && git commit"
        }
        None => {
            "transparency witness: unavailable; restore local Git and verify transparency-head-log.jsonl"
        }
    }
}

fn transparency_dirty_source_diagnostic(commit: &str) -> String {
    format!(
        "terminal transparency candidate: observed dirty source checkout at commit {commit}, expected a clean checkout bound to that commit; restore the checkout and retry"
    )
}

fn transparency_head_log_guard_diagnostic(row: &str) -> String {
    format!(
        "terminal transparency witness: observed prior uncommitted {row}, expected the previous row committed before another publication; commit transparency-head-log.jsonl and retry"
    )
}

fn transparency_last_head_row(root: &Path) -> String {
    std::fs::read(root.join("transparency-head-log.jsonl"))
        .ok()
        .and_then(|bytes| {
            bytes
                .split_inclusive(|byte| *byte == b'\n')
                .rfind(|line| !line.is_empty())
                .and_then(|line| {
                    serde_json::from_slice::<xtask::transparency_format::TransparencyHeadLogRow>(
                        line,
                    )
                    .ok()
                })
        })
        .map(|row| {
            format!(
                "row product={} seq={} version={} entry_sha256={}",
                row.product, row.seq, row.version, row.entry_sha256
            )
        })
        .unwrap_or_else(|| "head-log row with unreadable identity".to_owned())
}

fn cmd_rust_release_manifest_advisory_config(args: &[String]) -> ExitCode {
    let mut database_root = None;
    let mut output = None;
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--db-root" if database_root.is_none() => {
                let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else {
                    return advisory_config_usage();
                };
                database_root = Some(PathBuf::from(value));
                index += 2;
            }
            "--out" if output.is_none() => {
                let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else {
                    return advisory_config_usage();
                };
                output = Some(PathBuf::from(value));
                index += 2;
            }
            _ => return advisory_config_usage(),
        }
    }
    let (Some(database_root), Some(mut output)) = (database_root, output) else {
        return advisory_config_usage();
    };
    let mirror_locator = match std::env::var("SOLSTONE_ADVISORY_MIRROR_LOCATOR")
        .map_err(|_| xtask::release_advisory::AdvisoryError::MirrorLocatorInvalid)
        .and_then(|locator| {
            xtask::release_advisory::validate_mirror_locator(&locator).map(|()| locator)
        }) {
        Ok(locator) => locator,
        Err(error) => {
            eprintln!("rust release advisory config failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !database_root.is_absolute() {
        eprintln!(
            "rust release advisory config failed: --db-root is not absolute; pass the mapped target/release-advisory-db path and retry"
        );
        return ExitCode::FAILURE;
    }
    let root = repo_root();
    if !output.is_absolute() {
        output = root.join(output);
    }
    match xtask::release_advisory::materialize_advisory_config_at(
        &root,
        &database_root,
        &output,
        &mirror_locator,
    ) {
        Ok(_) => {
            println!("rust release advisory config: deterministic offline policy materialized");
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rust release advisory config failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn advisory_config_usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask rust-release-manifest advisory-config --db-root <isolated-absolute-path> --out <path>"
    );
    ExitCode::from(2)
}

fn cmd_rust_release_manifest_finalize(args: &[String]) -> ExitCode {
    let mut expected_commit = None;
    let mut sign = false;
    let mut delta_base_fulls = Vec::new();
    let mut index = 2;
    while index < args.len() {
        match args[index].as_str() {
            "--expected-release-commit" if expected_commit.is_none() => {
                let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else {
                    return finalize_usage();
                };
                expected_commit = Some(value.clone());
                index += 2;
            }
            "--sign" if !sign => {
                sign = true;
                index += 1;
            }
            "--delta-base-full" => {
                let Some(value) = args.get(index + 1).filter(|value| !value.is_empty()) else {
                    return finalize_usage();
                };
                delta_base_fulls.push(value.clone());
                index += 2;
            }
            _ => return finalize_usage(),
        }
    }
    let Some(expected_release_commit) = expected_commit else {
        return finalize_usage();
    };

    let mut selection_record = Vec::new();
    if std::io::stdin().read_to_end(&mut selection_record).is_err() || selection_record.is_empty() {
        eprintln!(
            "rust release finalizer failed: selection stdin is empty or unreadable; pipe the exact preflight selection JSON into finalize"
        );
        return ExitCode::FAILURE;
    }
    let Some(git_program) = std::env::var_os("GIT").map(PathBuf::from) else {
        eprintln!(
            "rust release finalizer failed: GIT is not an absolute selected executable path; set GIT to the local Git executable and retry"
        );
        return ExitCode::FAILURE;
    };
    if !git_program.is_absolute() {
        eprintln!(
            "rust release finalizer failed: GIT is not absolute; set it to the exact local Git executable and retry"
        );
        return ExitCode::FAILURE;
    }
    let Some(advisory_tree_sha256) =
        std::env::var("SOLSTONE_ADVISORY_TREE_SHA256")
            .ok()
            .filter(|value| {
                value.len() == 64
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            })
    else {
        eprintln!(
            "rust release finalizer failed: SOLSTONE_ADVISORY_TREE_SHA256 is missing or is not 64 lowercase hex; supply the reviewed isolated RustSec archive digest and retry"
        );
        return ExitCode::FAILURE;
    };
    let signing_keypair_alias = if sign {
        std::env::var("SM_KEYPAIR_ALIAS")
            .ok()
            .filter(|value| !value.is_empty())
    } else {
        None
    };
    let mirror_locator = match std::env::var("SOLSTONE_ADVISORY_MIRROR_LOCATOR")
        .map_err(|_| xtask::release_advisory::AdvisoryError::MirrorLocatorInvalid)
        .and_then(|locator| {
            xtask::release_advisory::validate_mirror_locator(&locator).map(|()| locator)
        }) {
        Ok(locator) => locator,
        Err(error) => {
            eprintln!("rust release finalizer failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let freshness_receipt = match std::env::var("SOLSTONE_ADVISORY_RECEIPT")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(xtask::release_advisory::AdvisoryError::FreshnessReceiptPathInvalid)
        .and_then(|path| {
            xtask::release_advisory::validate_freshness_receipt_path(&path).map(|()| path)
        }) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("rust release finalizer failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let mirror_public_key = match std::env::var("SOLSTONE_ADVISORY_MIRROR_PUB")
        .ok()
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or(xtask::release_advisory::AdvisoryError::MirrorPublicKeyPathInvalid)
        .and_then(|path| {
            xtask::release_advisory::validate_mirror_public_key_path(&path).map(|()| path)
        }) {
        Ok(path) => path,
        Err(error) => {
            eprintln!("rust release finalizer failed: {error}");
            return ExitCode::FAILURE;
        }
    };
    let Some(minisign_program) = resolve_path_program("minisign") else {
        eprintln!(
            "rust release finalizer failed: advisory mirror signature verifier is unavailable; install minisign on PATH and retry"
        );
        return ExitCode::FAILURE;
    };
    let request = xtask::release_finalizer::FinalizeRequest {
        expected_release_commit,
        sign_mode: if sign {
            xtask::release_selection::SelectionMode::Signed
        } else {
            xtask::release_selection::SelectionMode::Unsigned
        },
        selection_record,
        delta_base_fulls,
    };
    let root = repo_root();
    let runner = xtask::release_exec::ProcessCommandRunner;
    let clock = xtask::release_clock::SystemClock;
    let runtime = xtask::release_finalizer::FinalizeRuntime {
        checkout_root: &root,
        git_program: &git_program,
        advisory_tree_sha256: &advisory_tree_sha256,
        signing_keypair_alias: signing_keypair_alias.as_deref(),
        mirror_locator,
        freshness_receipt,
        mirror_public_key,
        minisign_program,
        mirror_public_key_sha256: xtask::release_advisory::MIRROR_MINISIGN_PUBKEY_SHA256.to_owned(),
    };
    match xtask::release_finalizer::finalize(runtime, &request, &runner, &clock) {
        Ok(result) => {
            println!(
                "rust release finalizer: promoted {} and {} ({})",
                result.candidate_relative_path, result.receipt_relative_path, result.signing_mode
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rust release finalizer failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn finalize_usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask rust-release-manifest finalize --expected-release-commit <40hex> [--sign] [--delta-base-full <Solstone-SEMVER-full.nupkg> ...] < selection.json"
    );
    ExitCode::from(2)
}

fn cmd_rust_release_manifest_prove_native(args: &[String]) -> ExitCode {
    let release_dir = match args {
        [_, command, flag, value]
            if command == "prove-native" && flag == "--release-dir" && !value.is_empty() =>
        {
            PathBuf::from(value)
        }
        _ => return prove_native_usage(),
    };
    let root = repo_root();
    let release_dir = if release_dir.is_absolute() {
        release_dir
    } else {
        root.join(release_dir)
    };
    let cargo = xtask::version_gate::configured_cargo();
    let git = std::env::var_os("GIT").unwrap_or_else(|| "git".into());
    let facts = match xtask::rust_release_manifest::gather_checkout_facts(&root, &cargo, &git) {
        Ok(facts) => facts,
        Err(error) => {
            eprintln!(
                "rust release native proof failed: checkout facts could not be established ({error}); restore the clean candidate source checkout and retry"
            );
            return ExitCode::FAILURE;
        }
    };
    let powershell = std::env::var_os("SOLSTONE_PROOF_POWERSHELL")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "powershell".into());
    let runner = xtask::release_exec::ProcessCommandRunner;
    let clock = xtask::release_clock::SystemClock;
    let runtime = xtask::native_release_proof::NativeProofRuntime {
        checkout_root: &root,
        facts: &facts,
        powershell_bootstrap: &powershell,
    };
    match xtask::native_release_proof::prove_native(runtime, &release_dir, &runner, &clock) {
        Ok(result) => {
            println!(
                "rust release native proof: verified candidate {} and wrote {}",
                result.version, result.receipt_relative_path
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("rust release native proof failed: {error}");
            ExitCode::FAILURE
        }
    }
}

fn prove_native_usage() -> ExitCode {
    eprintln!(
        "usage: cargo xtask rust-release-manifest prove-native --release-dir <target/release-candidate/VERSION>"
    );
    ExitCode::from(2)
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

fn cmd_dev() -> ExitCode {
    eprintln!("xtask dev: developer launcher (not yet implemented)");
    ExitCode::SUCCESS
}

#[cfg(test)]
mod transparency_cli_tests {
    use super::*;
    use xtask::release_exec::test_support::{FakeCommand, FakeCommandRunner};
    use xtask::release_exec::CommandOutput;

    #[cfg(not(windows))]
    const GIT: &str = "/selected/git";
    #[cfg(windows)]
    const GIT: &str = r"C:\selected\git.exe";
    #[cfg(not(windows))]
    const ROOT: &str = "/checkout";
    #[cfg(windows)]
    const ROOT: &str = r"C:\checkout";

    #[test]
    fn staged_transparency_head_row_is_uncommitted() {
        let base = vec!["-C".to_owned(), ROOT.to_owned(), "diff".to_owned()];
        let unstaged = [
            base.clone(),
            vec![
                "--quiet".to_owned(),
                "--".to_owned(),
                "transparency-head-log.jsonl".to_owned(),
            ],
        ]
        .concat();
        let staged = [
            base,
            vec![
                "--cached".to_owned(),
                "--quiet".to_owned(),
                "--".to_owned(),
                "transparency-head-log.jsonl".to_owned(),
            ],
        ]
        .concat();
        let runner = FakeCommandRunner::new(vec![
            FakeCommand::output(
                PathBuf::from(GIT),
                unstaged,
                CommandOutput {
                    status: 0,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            ),
            FakeCommand::output(
                PathBuf::from(GIT),
                staged,
                CommandOutput {
                    status: 1,
                    stdout: Vec::new(),
                    stderr: Vec::new(),
                },
            ),
        ]);
        assert_eq!(
            transparency_head_log_dirty(Path::new(ROOT), Path::new(GIT), &runner),
            Some(true)
        );
        assert_eq!(runner.remaining(), Ok(0));
    }

    #[test]
    fn transparency_head_log_summaries_cover_all_probe_states() {
        for (dirty, expected) in [
            (
                Some(false),
                "transparency witness: row present and committed",
            ),
            (
                Some(true),
                "transparency witness: row written uncommitted; run git add transparency-head-log.jsonl && git commit",
            ),
            (
                None,
                "transparency witness: unavailable; restore local Git and verify transparency-head-log.jsonl",
            ),
        ] {
            assert_eq!(transparency_head_log_summary(dirty), expected);
        }
    }

    #[test]
    fn transparency_head_log_blocking_guard_is_distinct_from_routine_notice() {
        let row = "row product=solstone-windows seq=7 version=0.2.17 entry_sha256=abc";
        let guard = transparency_head_log_guard_diagnostic(row);
        assert_eq!(
            guard,
            "terminal transparency witness: observed prior uncommitted row product=solstone-windows seq=7 version=0.2.17 entry_sha256=abc, expected the previous row committed before another publication; commit transparency-head-log.jsonl and retry"
        );
        assert_ne!(guard, transparency_head_log_summary(Some(true)));
    }

    #[test]
    fn transparency_dirty_source_diagnostic_names_commit_and_state() {
        let commit = "a".repeat(40);
        let diagnostic = transparency_dirty_source_diagnostic(&commit);
        assert_eq!(
            diagnostic,
            format!(
                "terminal transparency candidate: observed dirty source checkout at commit {commit}, expected a clean checkout bound to that commit; restore the checkout and retry"
            )
        );
        assert!(diagnostic.contains("dirty source checkout"));
        assert!(diagnostic.contains(&commit));
    }
}
