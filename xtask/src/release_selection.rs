// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Closed consumer contract for resolver-selected release tools and actions.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::artifact_fs::child_process_path_text;
use crate::rust_release_manifest::{self, ReleaseToolProjection};

pub const SELECTION_SCHEMA: &str = "solstone.release-tool-selection.v1";

#[derive(Clone, Copy, Debug, Deserialize, Serialize, Eq, PartialEq)]
#[serde(rename_all = "lowercase")]
pub enum SelectionMode {
    Unsigned,
    Signed,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseToolSelection {
    pub schema: String,
    pub mode: SelectionMode,
    pub tools: SelectedTools,
    pub actions: SelectedActions,
    pub msvc_environment: MsvcEnvironmentDelta,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedTools {
    pub rustc: RustcTool,
    pub cargo: VersionTool,
    #[serde(rename = "cargo-deny")]
    pub cargo_deny: VersionTool,
    pub dotnet: VersionTool,
    pub vpk: VpkTool,
    pub node: VersionTool,
    pub npm: VersionTool,
    #[serde(rename = "msvc-cl")]
    pub msvc_cl: MsvcTool,
    #[serde(rename = "windows-sdk")]
    pub windows_sdk: VersionTool,
    pub powershell: VersionTool,
    pub smctl: Option<VersionTool>,
    pub signtool: Option<SignTool>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct RustcTool {
    pub path: PathBuf,
    pub version: String,
    pub host: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VersionTool {
    pub path: PathBuf,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct VpkTool {
    pub path: PathBuf,
    pub version: String,
    #[serde(rename = "packageId")]
    pub package_id: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MsvcTool {
    pub path: PathBuf,
    #[serde(rename = "compilerVersion")]
    pub compiler_version: String,
    #[serde(rename = "toolsetVersion")]
    pub toolset_version: String,
    pub host: String,
    pub target: String,
    #[serde(rename = "vcvarsallPath")]
    pub vcvarsall_path: PathBuf,
    #[serde(rename = "vcvarsVersionArg")]
    pub vcvars_version_arg: String,
    #[serde(rename = "installationPath")]
    pub installation_path: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SignTool {
    pub path: PathBuf,
    pub version: String,
    #[serde(rename = "originalFilename")]
    pub original_filename: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedAction {
    pub program: PathBuf,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct SelectedActions {
    pub npm_ci: SelectedAction,
    pub npm_build: SelectedAction,
    pub cargo_release_build: SelectedAction,
    pub signing_auth_preflight: Option<SelectedAction>,
    pub vpk_pack: SelectedAction,
    pub smctl_sign: Option<SelectedAction>,
    pub signtool_verify: Option<SelectedAction>,
    pub cargo_deny_advisories: SelectedAction,
    pub native_smoke: SelectedAction,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct MsvcEnvironmentDelta {
    #[serde(rename = "PATH")]
    pub path: String,
    #[serde(rename = "INCLUDE")]
    pub include: String,
    #[serde(rename = "LIB")]
    pub lib: String,
    #[serde(rename = "LIBPATH")]
    pub libpath: String,
    #[serde(rename = "VCINSTALLDIR")]
    pub vc_install_dir: String,
    #[serde(rename = "VCToolsInstallDir")]
    pub vc_tools_install_dir: String,
    #[serde(rename = "VCToolsVersion")]
    pub vc_tools_version: String,
    #[serde(rename = "UniversalCRTSdkDir")]
    pub universal_crt_sdk_dir: String,
    #[serde(rename = "UCRTVersion")]
    pub ucrt_version: String,
    #[serde(rename = "WindowsSdkDir")]
    pub windows_sdk_dir: String,
    #[serde(rename = "WindowsSdkBinPath")]
    pub windows_sdk_bin_path: String,
    #[serde(rename = "WindowsLibPath")]
    pub windows_lib_path: String,
    #[serde(rename = "WindowsSDKVersion")]
    pub windows_sdk_version: String,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ManifestSafeToolProjection {
    pub rustc_verbose: String,
    pub cargo_version: String,
    pub cargo_deny_version: String,
    pub native_tools: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionError {
    MalformedRecord,
    SchemaMismatch,
    SignedToolSetMismatch,
    ToolIdentityInvalid { tool: &'static str },
    ToolPathNotAbsolute { tool: &'static str },
    SignedActionSetMismatch,
    ActionPathNotAbsolute { action: &'static str },
    ActionToolPathMismatch { action: &'static str },
    ActionArgvMismatch { action: &'static str },
    UndocumentedPlaceholder { action: &'static str },
    MsvcEnvironmentInvalid,
    SigningChildEnvironmentInvalid,
    ToolchainAuthorityInvalid,
    SelectedIdentityMismatch { tool: &'static str },
}

impl fmt::Display for SelectionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MalformedRecord => write!(
                formatter,
                "release-tool selection JSON is malformed, incomplete, or has an unknown field; rerun the pinned release-tool preflight"
            ),
            Self::SchemaMismatch => write!(
                formatter,
                "release-tool selection schema is not solstone.release-tool-selection.v1; rerun the current release-tool preflight"
            ),
            Self::SignedToolSetMismatch => write!(
                formatter,
                "release-tool selection has the wrong signed-only tool set; rerun preflight in the requested signed or unsigned mode"
            ),
            Self::ToolIdentityInvalid { tool } => write!(
                formatter,
                "selected {tool} identity is incomplete or inconsistent; rerun release-tool preflight and use its record unchanged"
            ),
            Self::ToolPathNotAbsolute { tool } => write!(
                formatter,
                "selected {tool} path is not absolute; rerun release-tool preflight and use its absolute path"
            ),
            Self::SignedActionSetMismatch => write!(
                formatter,
                "release action selection has the wrong signed-only action set; rerun preflight in the requested signed or unsigned mode"
            ),
            Self::ActionPathNotAbsolute { action } => write!(
                formatter,
                "selected {action} program is not absolute; rerun release-tool preflight and use its absolute action path"
            ),
            Self::ActionToolPathMismatch { action } => write!(
                formatter,
                "selected {action} program disagrees with its selected tool; rerun preflight and pass the selection record unchanged"
            ),
            Self::ActionArgvMismatch { action } => write!(
                formatter,
                "selected {action} argv differs from the closed release template; rerun the current preflight and do not reconstruct argv"
            ),
            Self::UndocumentedPlaceholder { action } => write!(
                formatter,
                "selected {action} argv contains an undocumented placeholder; use only the placeholders emitted by current preflight"
            ),
            Self::MsvcEnvironmentInvalid => write!(
                formatter,
                "selected MSVC environment delta is incomplete or contains an invalid value; rerun the pinned vcvars preflight"
            ),
            Self::SigningChildEnvironmentInvalid => write!(
                formatter,
                "selected signing tools cannot form the required child environment; rerun signed preflight and pass its record unchanged"
            ),
            Self::ToolchainAuthorityInvalid => write!(
                formatter,
                "committed release-toolchain authority could not be projected; restore packaging/release-toolchain.json and retry"
            ),
            Self::SelectedIdentityMismatch { tool } => write!(
                formatter,
                "selected {tool} identity disagrees with the committed toolchain authority; rerun the pinned preflight before retrying"
            ),
        }
    }
}

impl std::error::Error for SelectionError {}

impl ReleaseToolSelection {
    pub fn parse(bytes: &[u8]) -> Result<Self, SelectionError> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|_| SelectionError::MalformedRecord)?;
        validate_mode_specific_presence(&value)?;
        let selection: Self =
            serde_json::from_value(value).map_err(|_| SelectionError::MalformedRecord)?;
        selection.validate()?;
        Ok(selection)
    }

    pub fn sanitized_projection(
        &self,
        checkout_root: &Path,
    ) -> Result<ManifestSafeToolProjection, SelectionError> {
        let projection = rust_release_manifest::project_release_toolchain(checkout_root)
            .map_err(|_| SelectionError::ToolchainAuthorityInvalid)?;
        self.validate_selected_identities(&projection)?;
        let native_tools = match self.mode {
            SelectionMode::Unsigned => projection.unsigned_native_tools,
            SelectionMode::Signed => projection.signed_native_tools,
        };
        Ok(ManifestSafeToolProjection {
            rustc_verbose: projection.rustc_verbose,
            cargo_version: projection.cargo_version,
            cargo_deny_version: projection.cargo_deny_version,
            native_tools,
        })
    }

    pub fn msvc_env_overlay(&self) -> BTreeMap<String, String> {
        BTreeMap::from([
            ("INCLUDE".to_owned(), self.msvc_environment.include.clone()),
            ("LIB".to_owned(), self.msvc_environment.lib.clone()),
            ("LIBPATH".to_owned(), self.msvc_environment.libpath.clone()),
            ("PATH".to_owned(), self.msvc_environment.path.clone()),
            (
                "UCRTVersion".to_owned(),
                self.msvc_environment.ucrt_version.clone(),
            ),
            (
                "UniversalCRTSdkDir".to_owned(),
                self.msvc_environment.universal_crt_sdk_dir.clone(),
            ),
            (
                "VCINSTALLDIR".to_owned(),
                self.msvc_environment.vc_install_dir.clone(),
            ),
            (
                "VCToolsInstallDir".to_owned(),
                self.msvc_environment.vc_tools_install_dir.clone(),
            ),
            (
                "VCToolsVersion".to_owned(),
                self.msvc_environment.vc_tools_version.clone(),
            ),
            (
                "WindowsLibPath".to_owned(),
                self.msvc_environment.windows_lib_path.clone(),
            ),
            (
                "WindowsSDKVersion".to_owned(),
                self.msvc_environment.windows_sdk_version.clone(),
            ),
            (
                "WindowsSdkBinPath".to_owned(),
                self.msvc_environment.windows_sdk_bin_path.clone(),
            ),
            (
                "WindowsSdkDir".to_owned(),
                self.msvc_environment.windows_sdk_dir.clone(),
            ),
        ])
    }

    pub fn signing_child_env_overlay(&self) -> Result<BTreeMap<String, String>, SelectionError> {
        let signtool = self
            .tools
            .signtool
            .as_ref()
            .ok_or(SelectionError::SigningChildEnvironmentInvalid)?;
        let parent = signtool
            .path
            .parent()
            .ok_or(SelectionError::SigningChildEnvironmentInvalid)?;
        let parent = child_process_path_text(parent)
            .ok_or(SelectionError::SigningChildEnvironmentInvalid)?;
        Ok(BTreeMap::from([(
            "PATH".to_owned(),
            format!("{parent};{}", self.msvc_environment.path),
        )]))
    }

    fn validate(&self) -> Result<(), SelectionError> {
        if self.schema != SELECTION_SCHEMA {
            return Err(SelectionError::SchemaMismatch);
        }
        match self.mode {
            SelectionMode::Unsigned => {
                if self.tools.smctl.is_some() || self.tools.signtool.is_some() {
                    return Err(SelectionError::SignedToolSetMismatch);
                }
            }
            SelectionMode::Signed => {
                if self.tools.smctl.is_none() || self.tools.signtool.is_none() {
                    return Err(SelectionError::SignedToolSetMismatch);
                }
            }
        }
        self.validate_tool_fields()?;
        self.validate_msvc_environment()?;
        self.validate_actions()
    }

    fn validate_tool_fields(&self) -> Result<(), SelectionError> {
        validate_absolute("rustc", &self.tools.rustc.path)?;
        validate_nonempty(
            "rustc",
            &[&self.tools.rustc.version, &self.tools.rustc.host],
        )?;
        for (name, tool) in [
            ("cargo", &self.tools.cargo),
            ("cargo-deny", &self.tools.cargo_deny),
            ("dotnet", &self.tools.dotnet),
            ("node", &self.tools.node),
            ("npm", &self.tools.npm),
            ("powershell", &self.tools.powershell),
            ("windows-sdk", &self.tools.windows_sdk),
        ] {
            validate_absolute(name, &tool.path)?;
            validate_nonempty(name, &[&tool.version])?;
        }
        validate_absolute("vpk", &self.tools.vpk.path)?;
        validate_nonempty(
            "vpk",
            &[&self.tools.vpk.version, &self.tools.vpk.package_id],
        )?;
        validate_absolute("msvc-cl", &self.tools.msvc_cl.path)?;
        validate_absolute("msvc-cl", &self.tools.msvc_cl.vcvarsall_path)?;
        validate_absolute("msvc-cl", &self.tools.msvc_cl.installation_path)?;
        validate_nonempty(
            "msvc-cl",
            &[
                &self.tools.msvc_cl.compiler_version,
                &self.tools.msvc_cl.toolset_version,
                &self.tools.msvc_cl.host,
                &self.tools.msvc_cl.target,
                &self.tools.msvc_cl.vcvars_version_arg,
            ],
        )?;
        if self.tools.msvc_cl.vcvars_version_arg
            != format!("-vcvars_ver={}", self.tools.msvc_cl.toolset_version)
        {
            return Err(SelectionError::ToolIdentityInvalid { tool: "msvc-cl" });
        }
        if let Some(smctl) = &self.tools.smctl {
            validate_absolute("smctl", &smctl.path)?;
            validate_nonempty("smctl", &[&smctl.version])?;
        }
        if let Some(signtool) = &self.tools.signtool {
            validate_absolute("signtool", &signtool.path)?;
            validate_nonempty(
                "signtool",
                &[&signtool.version, &signtool.original_filename],
            )?;
        }
        Ok(())
    }

    fn validate_msvc_environment(&self) -> Result<(), SelectionError> {
        let env = &self.msvc_environment;
        let values = [
            &env.path,
            &env.include,
            &env.lib,
            &env.libpath,
            &env.vc_install_dir,
            &env.vc_tools_install_dir,
            &env.vc_tools_version,
            &env.universal_crt_sdk_dir,
            &env.ucrt_version,
            &env.windows_sdk_dir,
            &env.windows_sdk_bin_path,
            &env.windows_lib_path,
            &env.windows_sdk_version,
        ];
        if values.iter().any(|value| value.trim().is_empty()) {
            return Err(SelectionError::MsvcEnvironmentInvalid);
        }
        Ok(())
    }

    fn validate_actions(&self) -> Result<(), SelectionError> {
        match self.mode {
            SelectionMode::Unsigned => {
                if self.actions.signing_auth_preflight.is_some()
                    || self.actions.smctl_sign.is_some()
                    || self.actions.signtool_verify.is_some()
                {
                    return Err(SelectionError::SignedActionSetMismatch);
                }
            }
            SelectionMode::Signed => {
                if self.actions.signing_auth_preflight.is_none()
                    || self.actions.smctl_sign.is_none()
                    || self.actions.signtool_verify.is_none()
                {
                    return Err(SelectionError::SignedActionSetMismatch);
                }
            }
        }

        validate_action(
            "npm_ci",
            &self.actions.npm_ci,
            &self.tools.npm.path,
            &["--prefix", "ui", "ci", "--offline"],
            &[],
        )?;
        validate_action(
            "npm_build",
            &self.actions.npm_build,
            &self.tools.npm.path,
            &["--prefix", "ui", "run", "build"],
            &[],
        )?;
        validate_action(
            "cargo_release_build",
            &self.actions.cargo_release_build,
            &self.tools.cargo.path,
            &[
                "build",
                "--locked",
                "-p",
                "solstone-windows-app",
                "--release",
                "--features",
                "custom-protocol",
            ],
            &[],
        )?;
        validate_action(
            "vpk_pack",
            &self.actions.vpk_pack,
            &self.tools.vpk.path,
            &[
                "pack",
                "--packId",
                "Solstone",
                "--packVersion",
                "{version}",
                "--packDir",
                "{stage_dir}",
                "--mainExe",
                "solstone-windows-app.exe",
                "--outputDir",
                "{output_dir}",
                "--packTitle",
                "sol",
                "--packAuthors",
                "sol pbc",
                "--icon",
                "src-tauri/icons/icon.ico",
                "--channel",
                "win",
                "--framework",
                "webview2",
                "--releaseNotes",
                "{release_notes}",
            ],
            &[
                "{version}",
                "{stage_dir}",
                "{output_dir}",
                "{release_notes}",
            ],
        )?;
        validate_action(
            "cargo_deny_advisories",
            &self.actions.cargo_deny_advisories,
            &self.tools.cargo.path,
            &[
                "deny",
                "--locked",
                "--offline",
                "--config",
                "{advisory_config}",
                "check",
                "advisories",
            ],
            &["{advisory_config}"],
        )?;
        validate_action(
            "native_smoke",
            &self.actions.native_smoke,
            &self.tools.powershell.path,
            &[
                "-NoProfile",
                "-ExecutionPolicy",
                "Bypass",
                "-File",
                "scripts/smoke.ps1",
                "-AppExe",
                "{installed_exe}",
                "-ExpectedVersion",
                "{expected_version}",
                "-ExpectedSha256",
                "{expected_sha256}",
                "-DisableInstalledFallback",
                "-DotnetPath",
                "{dotnet_path}",
            ],
            &[
                "{installed_exe}",
                "{expected_version}",
                "{expected_sha256}",
                "{dotnet_path}",
            ],
        )?;

        if let (Some(auth), Some(smctl), Some(verify), Some(smctl_tool), Some(signtool_tool)) = (
            &self.actions.signing_auth_preflight,
            &self.actions.smctl_sign,
            &self.actions.signtool_verify,
            &self.tools.smctl,
            &self.tools.signtool,
        ) {
            validate_action(
                "signing_auth_preflight",
                auth,
                &self.tools.powershell.path,
                &[
                    "-NoProfile",
                    "-ExecutionPolicy",
                    "Bypass",
                    "-File",
                    "packaging/signing/preflight-auth.ps1",
                    "-SmctlPath",
                    "{smctl_path}",
                ],
                &["{smctl_path}"],
            )?;
            validate_action(
                "smctl_sign",
                smctl,
                &smctl_tool.path,
                &[
                    "sign",
                    "--keypair-alias",
                    "{keypair_alias}",
                    "--input",
                    "{file}",
                ],
                &["{keypair_alias}", "{file}"],
            )?;
            validate_action(
                "signtool_verify",
                verify,
                &signtool_tool.path,
                &["verify", "/pa", "/all", "/v", "{file}"],
                &["{file}"],
            )?;
        }
        Ok(())
    }

    fn validate_selected_identities(
        &self,
        projection: &ReleaseToolProjection,
    ) -> Result<(), SelectionError> {
        let rustc_expected = format!(
            "release: {}\nhost: {}",
            self.tools.rustc.version, self.tools.rustc.host
        );
        require_identity("rustc", rustc_expected == projection.rustc_verbose)?;
        require_identity(
            "cargo",
            self.tools.cargo.version == projection.cargo_version,
        )?;
        require_identity(
            "cargo-deny",
            self.tools.cargo_deny.version == projection.cargo_deny_version,
        )?;
        let unsigned = &projection.unsigned_native_tools;
        require_projection_identity("dotnet", &self.tools.dotnet.version, unsigned)?;
        require_projection_identity("vpk", &self.tools.vpk.version, unsigned)?;
        require_identity("vpk", self.tools.vpk.package_id == "vpk")?;
        require_projection_identity("node", &self.tools.node.version, unsigned)?;
        require_projection_identity("npm", &self.tools.npm.version, unsigned)?;
        require_projection_identity("windows-sdk", &self.tools.windows_sdk.version, unsigned)?;
        require_projection_identity("powershell", &self.tools.powershell.version, unsigned)?;
        let msvc = format!(
            "{} toolset {} {}->{}",
            self.tools.msvc_cl.compiler_version,
            self.tools.msvc_cl.toolset_version,
            self.tools.msvc_cl.host,
            self.tools.msvc_cl.target
        );
        require_projection_identity("msvc-cl", &msvc, unsigned)?;

        if self.mode == SelectionMode::Signed {
            let signed = &projection.signed_native_tools;
            let smctl = self
                .tools
                .smctl
                .as_ref()
                .ok_or(SelectionError::SignedToolSetMismatch)?;
            require_projection_identity("smctl", &smctl.version, signed)?;
            let signtool = self
                .tools
                .signtool
                .as_ref()
                .ok_or(SelectionError::SignedToolSetMismatch)?;
            require_projection_identity(
                "signtool",
                &format!("productVersion {}", signtool.version),
                signed,
            )?;
            require_identity("signtool", signtool.original_filename == "SIGNTOOL.EXE")?;
        }
        Ok(())
    }
}

fn validate_mode_specific_presence(value: &Value) -> Result<(), SelectionError> {
    let top = value.as_object().ok_or(SelectionError::MalformedRecord)?;
    let mode = top
        .get("mode")
        .and_then(Value::as_str)
        .ok_or(SelectionError::MalformedRecord)?;
    let tools = top
        .get("tools")
        .and_then(Value::as_object)
        .ok_or(SelectionError::MalformedRecord)?;
    let actions = top
        .get("actions")
        .and_then(Value::as_object)
        .ok_or(SelectionError::MalformedRecord)?;
    let signed_tools_present = ["smctl", "signtool"]
        .iter()
        .map(|name| tools.contains_key(*name));
    let signed_actions_present = ["signing_auth_preflight", "smctl_sign", "signtool_verify"]
        .iter()
        .map(|name| actions.contains_key(*name));
    match mode {
        "unsigned" => {
            if signed_tools_present.clone().any(|present| present) {
                return Err(SelectionError::SignedToolSetMismatch);
            }
            if signed_actions_present.clone().any(|present| present) {
                return Err(SelectionError::SignedActionSetMismatch);
            }
        }
        "signed" => {
            if !signed_tools_present.clone().all(|present| present) {
                return Err(SelectionError::SignedToolSetMismatch);
            }
            if !signed_actions_present.clone().all(|present| present) {
                return Err(SelectionError::SignedActionSetMismatch);
            }
        }
        _ => return Err(SelectionError::MalformedRecord),
    }
    Ok(())
}

fn validate_action(
    name: &'static str,
    action: &SelectedAction,
    expected_program: &Path,
    expected_argv: &[&str],
    allowed_placeholders: &[&str],
) -> Result<(), SelectionError> {
    if !is_absolute_selection_path(&action.program) {
        return Err(SelectionError::ActionPathNotAbsolute { action: name });
    }
    if action.program != expected_program {
        return Err(SelectionError::ActionToolPathMismatch { action: name });
    }
    for arg in &action.argv {
        if (arg.contains('{') || arg.contains('}')) && !allowed_placeholders.contains(&arg.as_str())
        {
            return Err(SelectionError::UndocumentedPlaceholder { action: name });
        }
    }
    if action.argv.len() != expected_argv.len()
        || action
            .argv
            .iter()
            .map(String::as_str)
            .ne(expected_argv.iter().copied())
    {
        return Err(SelectionError::ActionArgvMismatch { action: name });
    }
    Ok(())
}

fn validate_absolute(tool: &'static str, path: &Path) -> Result<(), SelectionError> {
    if !is_absolute_selection_path(path) {
        return Err(SelectionError::ToolPathNotAbsolute { tool });
    }
    Ok(())
}

fn validate_nonempty(tool: &'static str, values: &[&String]) -> Result<(), SelectionError> {
    if values.iter().any(|value| value.trim().is_empty()) {
        return Err(SelectionError::ToolIdentityInvalid { tool });
    }
    Ok(())
}

fn is_absolute_selection_path(path: &Path) -> bool {
    if path.is_absolute() {
        return true;
    }
    let value = path.to_string_lossy().as_bytes().to_vec();
    (value.len() >= 3
        && value[0].is_ascii_alphabetic()
        && value[1] == b':'
        && matches!(value[2], b'\\' | b'/'))
        || value.starts_with(b"\\\\")
        || value.starts_with(b"//")
}

fn require_projection_identity(
    tool: &'static str,
    actual: &str,
    projected: &BTreeMap<String, String>,
) -> Result<(), SelectionError> {
    require_identity(
        tool,
        projected
            .get(tool)
            .is_some_and(|expected| expected == actual),
    )
}

fn require_identity(tool: &'static str, matches: bool) -> Result<(), SelectionError> {
    if matches {
        Ok(())
    } else {
        Err(SelectionError::SelectedIdentityMismatch { tool })
    }
}
