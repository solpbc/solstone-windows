// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::fs;
use std::path::Path;

use serde_json::{json, Value};
use xtask::release_selection::{ReleaseToolSelection, SelectionError, SelectionMode};

#[cfg(not(windows))]
const PRIVATE_ROOT: &str = "/private/operator/release-tools";
#[cfg(windows)]
const PRIVATE_ROOT: &str = r"C:\private\operator\release-tools";
#[cfg(not(windows))]
const OTHER_ROOT: &str = "/other";
#[cfg(windows)]
const OTHER_ROOT: &str = r"C:\other";

fn workspace_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("xtask has workspace parent")
}

fn action(program: &str, argv: &[&str]) -> Value {
    json!({ "program": program, "argv": argv })
}

fn valid_selection(mode: SelectionMode) -> Value {
    let cargo = format!("{PRIVATE_ROOT}/cargo.exe");
    let npm = format!("{PRIVATE_ROOT}/npm.cmd");
    let powershell = format!("{PRIVATE_ROOT}/powershell.exe");
    let vpk = format!("{PRIVATE_ROOT}/vpk.exe");
    let smctl = format!("{PRIVATE_ROOT}/smctl.exe");
    let signtool = format!("{PRIVATE_ROOT}/signtool.exe");
    let mut tools = json!({
        "rustc": {
            "path": format!("{PRIVATE_ROOT}/rustc.exe"),
            "version": "1.96.0",
            "host": "x86_64-pc-windows-msvc"
        },
        "cargo": { "path": cargo, "version": "1.96.0" },
        "cargo-deny": {
            "path": format!("{PRIVATE_ROOT}/cargo-deny.exe"),
            "version": "0.20.2"
        },
        "dotnet": {
            "path": format!("{PRIVATE_ROOT}/dotnet.exe"),
            "version": "8.0.422"
        },
        "vpk": { "path": vpk, "version": "1.2.0", "packageId": "vpk" },
        "node": {
            "path": format!("{PRIVATE_ROOT}/node.exe"),
            "version": "24.16.0"
        },
        "npm": { "path": npm, "version": "11.13.0" },
        "msvc-cl": {
            "path": format!("{PRIVATE_ROOT}/VC/Tools/MSVC/14.44.35207/bin/Hostx64/x64/cl.exe"),
            "compilerVersion": "19.44.35228",
            "toolsetVersion": "14.44.35207",
            "host": "x64",
            "target": "x64",
            "vcvarsallPath": format!("{PRIVATE_ROOT}/VC/Auxiliary/Build/vcvarsall.bat"),
            "vcvarsVersionArg": "-vcvars_ver=14.44.35207",
            "installationPath": format!("{PRIVATE_ROOT}/VisualStudio")
        },
        "windows-sdk": {
            "path": format!("{PRIVATE_ROOT}/WindowsKits/Lib/10.0.26100.0"),
            "version": "10.0.26100.0"
        },
        "powershell": { "path": powershell, "version": "5.1" }
    });

    let mut actions = json!({
        "npm_ci": action(&npm, &["--prefix", "ui", "ci", "--offline"]),
        "npm_build": action(&npm, &["--prefix", "ui", "run", "build"]),
        "cargo_release_build": action(
            &cargo,
            &[
                "build", "--locked", "-p", "solstone-windows-app", "--release",
                "--features", "custom-protocol"
            ]
        ),
        "vpk_pack": action(
            &vpk,
            &[
                "pack", "--packId", "Solstone", "--packVersion", "{version}",
                "--packDir", "{stage_dir}", "--mainExe", "solstone-windows-app.exe",
                "--outputDir", "{output_dir}", "--packTitle", "sol", "--packAuthors",
                "sol pbc", "--icon", "src-tauri/icons/icon.ico", "--channel", "win",
                "--framework", "webview2", "--releaseNotes", "{release_notes}"
            ]
        ),
        "cargo_deny_advisories": action(
            &cargo,
            &[
                "deny", "--locked", "--offline", "--config", "{advisory_config}",
                "check", "advisories"
            ]
        ),
        "native_smoke": action(
            &powershell,
            &[
                "-NoProfile", "-ExecutionPolicy", "Bypass", "-File", "scripts/smoke.ps1",
                "-AppExe", "{installed_exe}", "-ExpectedVersion", "{expected_version}",
                "-ExpectedSha256", "{expected_sha256}", "-DisableInstalledFallback",
                "-DotnetPath", "{dotnet_path}"
            ]
        )
    });

    if mode == SelectionMode::Signed {
        tools.as_object_mut().expect("tools object").insert(
            "smctl".to_owned(),
            json!({ "path": smctl, "version": "1.64.2" }),
        );
        tools.as_object_mut().expect("tools object").insert(
            "signtool".to_owned(),
            json!({
                "path": signtool,
                "version": "10.0.26100.7705",
                "originalFilename": "SIGNTOOL.EXE"
            }),
        );
        actions.as_object_mut().expect("actions object").insert(
            "signing_auth_preflight".to_owned(),
            action(
                &powershell,
                &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    "packaging/signing/preflight-auth.ps1",
                    "-SmctlPath",
                    "{smctl_path}",
                ],
            ),
        );
        actions.as_object_mut().expect("actions object").insert(
            "smctl_sign".to_owned(),
            action(
                &smctl,
                &[
                    "sign",
                    "--keypair-alias",
                    "{keypair_alias}",
                    "--input",
                    "{file}",
                ],
            ),
        );
        actions.as_object_mut().expect("actions object").insert(
            "signtool_verify".to_owned(),
            action(&signtool, &["verify", "/pa", "/all", "/v", "{file}"]),
        );
    }

    json!({
        "schema": "solstone.release-tool-selection.v1",
        "mode": match mode {
            SelectionMode::Unsigned => "unsigned",
            SelectionMode::Signed => "signed",
        },
        "tools": tools,
        "actions": actions,
        "msvc_environment": {
            "PATH": format!("{PRIVATE_ROOT}/VC/bin;{PRIVATE_ROOT}/WindowsKits/bin"),
            "INCLUDE": format!("{PRIVATE_ROOT}/VC/include"),
            "LIB": format!("{PRIVATE_ROOT}/VC/lib"),
            "LIBPATH": format!("{PRIVATE_ROOT}/VC/libpath"),
            "VCINSTALLDIR": format!("{PRIVATE_ROOT}/VC"),
            "VCToolsInstallDir": format!("{PRIVATE_ROOT}/VC/Tools/MSVC/14.44.35207"),
            "VCToolsVersion": "14.44.35207",
            "UniversalCRTSdkDir": format!("{PRIVATE_ROOT}/WindowsKits"),
            "UCRTVersion": "10.0.26100.0",
            "WindowsSdkDir": format!("{PRIVATE_ROOT}/WindowsKits"),
            "WindowsSdkBinPath": format!("{PRIVATE_ROOT}/WindowsKits/bin"),
            "WindowsLibPath": format!("{PRIVATE_ROOT}/WindowsKits/UnionMetadata"),
            "WindowsSDKVersion": "10.0.26100.0"
        }
    })
}

fn parse(value: &Value) -> Result<ReleaseToolSelection, SelectionError> {
    ReleaseToolSelection::parse(&serde_json::to_vec(value).expect("serialize test selection"))
}

#[test]
fn committed_action_templates_match_the_closed_selection_parser() {
    let contract: Value = serde_json::from_slice(
        &fs::read(workspace_root().join("packaging/release-toolchain.json"))
            .expect("read release-toolchain authority"),
    )
    .expect("parse release-toolchain authority");
    let signed_only = ["signing_auth_preflight", "smctl_sign", "signtool_verify"];

    for mode in [SelectionMode::Unsigned, SelectionMode::Signed] {
        let mut selection = valid_selection(mode);
        let mut emitted = serde_json::Map::new();
        for (name, template) in contract["selection"]["actions"]
            .as_object()
            .expect("action templates")
        {
            if mode == SelectionMode::Unsigned && signed_only.contains(&name.as_str()) {
                continue;
            }
            let tool = template["tool"].as_str().expect("action tool");
            let program = selection["tools"][tool]["path"]
                .as_str()
                .expect("selected action tool path");
            emitted.insert(
                name.clone(),
                json!({ "program": program, "argv": template["argv"].clone() }),
            );
        }
        selection["actions"] = Value::Object(emitted);
        parse(&selection).expect("committed action templates must pass the Rust authority");
    }

    let mut configured_environment: Vec<&str> = contract["selection"]["msvcEnvironment"]
        .as_array()
        .expect("MSVC environment allowlist")
        .iter()
        .map(|value| value.as_str().expect("environment name"))
        .collect();
    configured_environment.sort_unstable();
    let unsigned = valid_selection(SelectionMode::Unsigned);
    let mut parsed_environment: Vec<&str> = unsigned["msvc_environment"]
        .as_object()
        .expect("selection MSVC environment")
        .keys()
        .map(String::as_str)
        .collect();
    parsed_environment.sort_unstable();
    assert_eq!(configured_environment, parsed_environment);
}

#[test]
fn valid_signed_and_unsigned_records_parse_and_project_authority() {
    for mode in [SelectionMode::Unsigned, SelectionMode::Signed] {
        let selection = parse(&valid_selection(mode)).expect("parse valid selection");
        assert_eq!(selection.mode, mode);
        let projection = selection
            .sanitized_projection(workspace_root())
            .expect("project manifest-safe selected tools");
        assert_eq!(projection.cargo_version, "1.96.0");
        assert_eq!(
            projection
                .native_tools
                .get("signing_mode")
                .map(String::as_str),
            Some(match mode {
                SelectionMode::Unsigned => "unsigned",
                SelectionMode::Signed => "signed-verified",
            })
        );
    }
}

#[test]
fn unknown_fields_and_missing_or_extra_actions_are_rejected() {
    let mut unknown = valid_selection(SelectionMode::Unsigned);
    unknown
        .as_object_mut()
        .expect("selection object")
        .insert("operator_secret".to_owned(), json!("private"));
    assert_eq!(
        parse(&unknown).expect_err("unknown top field must fail"),
        SelectionError::MalformedRecord
    );

    let mut nested_unknown = valid_selection(SelectionMode::Unsigned);
    nested_unknown["actions"]["npm_ci"]["shell"] = json!("npm --prefix ui ci");
    assert_eq!(
        parse(&nested_unknown).expect_err("unknown action field must fail"),
        SelectionError::MalformedRecord
    );

    let mut missing = valid_selection(SelectionMode::Unsigned);
    missing["actions"]
        .as_object_mut()
        .expect("actions object")
        .remove("native_smoke");
    assert_eq!(
        parse(&missing).expect_err("missing common action must fail"),
        SelectionError::MalformedRecord
    );

    let mut extra = valid_selection(SelectionMode::Unsigned);
    extra["actions"]["legacy_publish"] = action(&format!("{PRIVATE_ROOT}/tool"), &["publish"]);
    assert_eq!(
        parse(&extra).expect_err("extra action must fail"),
        SelectionError::MalformedRecord
    );
}

#[test]
fn action_path_argv_and_placeholder_drift_are_distinct() {
    let mut disagreement = valid_selection(SelectionMode::Unsigned);
    disagreement["actions"]["npm_ci"]["program"] = json!(format!("{OTHER_ROOT}/npm.cmd"));
    assert_eq!(
        parse(&disagreement).expect_err("tool/action disagreement must fail"),
        SelectionError::ActionToolPathMismatch { action: "npm_ci" }
    );

    let mut argv_drift = valid_selection(SelectionMode::Unsigned);
    argv_drift["actions"]["npm_build"]["argv"][3] = json!("test");
    assert_eq!(
        parse(&argv_drift).expect_err("fixed argv drift must fail"),
        SelectionError::ActionArgvMismatch {
            action: "npm_build"
        }
    );

    let mut placeholder = valid_selection(SelectionMode::Unsigned);
    placeholder["actions"]["vpk_pack"]["argv"][4] = json!("{ambient_version}");
    assert_eq!(
        parse(&placeholder).expect_err("undocumented placeholder must fail"),
        SelectionError::UndocumentedPlaceholder { action: "vpk_pack" }
    );
}

#[test]
fn signed_only_actions_are_absent_in_unsigned_and_required_in_signed() {
    let mut unsigned = valid_selection(SelectionMode::Unsigned);
    unsigned["actions"]["signing_auth_preflight"] = action(
        &format!("{PRIVATE_ROOT}/powershell.exe"),
        &[
            "-NoProfile",
            "-ExecutionPolicy",
            "Bypass",
            "-File",
            "packaging/signing/preflight-auth.ps1",
            "-SmctlPath",
            "{smctl_path}",
        ],
    );
    assert_eq!(
        parse(&unsigned).expect_err("signed action in unsigned mode must fail"),
        SelectionError::SignedActionSetMismatch
    );

    let mut unsigned_null = valid_selection(SelectionMode::Unsigned);
    unsigned_null["tools"]["smctl"] = Value::Null;
    assert_eq!(
        parse(&unsigned_null).expect_err("present null signed tool must fail"),
        SelectionError::SignedToolSetMismatch
    );

    let mut signed = valid_selection(SelectionMode::Signed);
    signed["actions"]
        .as_object_mut()
        .expect("actions object")
        .remove("signtool_verify");
    assert_eq!(
        parse(&signed).expect_err("missing signed action must fail"),
        SelectionError::SignedActionSetMismatch
    );
}

#[test]
fn msvc_environment_rejects_every_non_allowlisted_key() {
    let mut selection = valid_selection(SelectionMode::Unsigned);
    selection["msvc_environment"]["SM_API_KEY"] = json!("must-not-enter-record");
    assert_eq!(
        parse(&selection).expect_err("credential-like environment key must fail"),
        SelectionError::MalformedRecord
    );
}

#[test]
fn sanitized_projection_contains_no_paths_argv_or_environment_values() {
    let selection = parse(&valid_selection(SelectionMode::Signed)).expect("parse signed record");
    let projection = selection
        .sanitized_projection(workspace_root())
        .expect("project safe values");
    let rendered = serde_json::to_string(&projection).expect("render safe projection");

    assert!(!rendered.contains(PRIVATE_ROOT));
    assert!(!rendered.contains("argv"));
    assert!(!rendered.contains("VCINSTALLDIR"));
    assert!(!rendered.contains("keypair_alias"));
    assert!(!rendered.contains("SM_API_KEY"));
    assert!(rendered.contains("signed-verified"));
}

#[test]
fn selected_identity_must_match_committed_projection() {
    let mut selection = valid_selection(SelectionMode::Unsigned);
    selection["tools"]["cargo"]["version"] = json!("9.9.9");
    let selection = parse(&selection).expect("structurally valid skewed selection");
    assert_eq!(
        selection
            .sanitized_projection(workspace_root())
            .expect_err("authority mismatch must fail"),
        SelectionError::SelectedIdentityMismatch { tool: "cargo" }
    );
}
