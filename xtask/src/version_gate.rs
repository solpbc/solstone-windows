// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Product-version authority and committed release-surface drift gate.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

const APP_PACKAGE: &str = "solstone-windows-app";
const TAURI_CONFIG: &str = "src-tauri/tauri.conf.json";
const WINGET_VERSION: &str = "packaging/winget/solpbc.Solstone.yaml";
const WINGET_INSTALLER: &str = "packaging/winget/solpbc.Solstone.installer.yaml";
const WINGET_LOCALE: &str = "packaging/winget/solpbc.Solstone.locale.en-US.yaml";
const SCOOP_MANIFEST: &str = "packaging/scoop/solstone.json";

#[derive(Clone, Copy)]
enum SurfaceParser {
    JsonVersion,
    WingetPackageVersion,
    WingetInstaller,
}

#[derive(Clone, Copy)]
struct VersionSurface {
    relative_file: &'static str,
    parser: SurfaceParser,
}

const VERSION_SURFACES: [VersionSurface; 5] = [
    VersionSurface {
        relative_file: TAURI_CONFIG,
        parser: SurfaceParser::JsonVersion,
    },
    VersionSurface {
        relative_file: WINGET_VERSION,
        parser: SurfaceParser::WingetPackageVersion,
    },
    VersionSurface {
        relative_file: WINGET_INSTALLER,
        parser: SurfaceParser::WingetInstaller,
    },
    VersionSurface {
        relative_file: WINGET_LOCALE,
        parser: SurfaceParser::WingetPackageVersion,
    },
    VersionSurface {
        relative_file: SCOOP_MANIFEST,
        parser: SurfaceParser::JsonVersion,
    },
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SurfaceMismatch {
    pub expected: String,
    pub actual: String,
    pub relative_file: &'static str,
    pub field: &'static str,
}

impl SurfaceMismatch {
    pub fn diagnostic(&self) -> String {
        format!(
            "ERROR: version-gate mismatch: expected {} from cargo metadata {}, actual {} at {} field {}.",
            self.expected, APP_PACKAGE, self.actual, self.relative_file, self.field
        )
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VersionGateError {
    Authority(String),
    Surface(Vec<SurfaceMismatch>),
}

pub fn configured_cargo() -> OsString {
    nonempty_env_os("SOLSTONE_VERSION_GATE_CARGO")
        .or_else(|| nonempty_env_os("CARGO"))
        .unwrap_or_else(|| OsString::from("cargo"))
}

pub fn run(root: &Path, cargo: &OsStr) -> Result<String, VersionGateError> {
    let version = authoritative_version(root, cargo).map_err(VersionGateError::Authority)?;
    let mut mismatches = Vec::new();

    for surface in VERSION_SURFACES {
        match surface.parser {
            SurfaceParser::JsonVersion => {
                check_json_version(root, surface.relative_file, &version, &mut mismatches)
            }
            SurfaceParser::WingetPackageVersion => {
                check_yaml_package_version(root, surface.relative_file, &version, &mut mismatches)
            }
            SurfaceParser::WingetInstaller => {
                check_yaml_package_version(root, surface.relative_file, &version, &mut mismatches);
                check_installer_basename(root, &version, &mut mismatches);
            }
        }
    }

    if mismatches.is_empty() {
        Ok(version)
    } else {
        Err(VersionGateError::Surface(mismatches))
    }
}

pub fn setup_exe_name(version: &str) -> String {
    format!("solstone-setup-{version}.exe")
}

fn nonempty_env_os(name: &str) -> Option<OsString> {
    std::env::var_os(name).filter(|value| !value.is_empty())
}

pub(crate) fn authoritative_version(root: &Path, cargo: &OsStr) -> Result<String, String> {
    let output = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1", "--locked"])
        .current_dir(root)
        .env("CARGO_NET_OFFLINE", "true")
        .output()
        .map_err(|error| format!("failed to run cargo metadata: {error}"))?;

    if !output.status.success() {
        let status = output.status.code().map_or_else(
            || "terminated by signal".to_string(),
            |code| code.to_string(),
        );
        let stderr = one_line(&String::from_utf8_lossy(&output.stderr));
        let detail = if stderr.is_empty() {
            String::new()
        } else {
            format!(": {stderr}")
        };
        return Err(format!("cargo metadata exited {status}{detail}"));
    }

    let metadata: Value = serde_json::from_slice(&output.stdout)
        .map_err(|error| format!("invalid cargo metadata JSON: {error}"))?;
    let packages = metadata
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| "cargo metadata omitted packages".to_string())?;
    let matching: Vec<_> = packages
        .iter()
        .filter(|package| package.get("name").and_then(Value::as_str) == Some(APP_PACKAGE))
        .collect();

    if matching.len() != 1 {
        return Err(format!(
            "cargo metadata matched {} packages named {APP_PACKAGE}",
            matching.len()
        ));
    }

    matching[0]
        .get("version")
        .and_then(Value::as_str)
        .filter(|version| !version.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| format!("cargo metadata package {APP_PACKAGE} omitted a non-empty version"))
}

fn check_json_version(
    root: &Path,
    relative_file: &'static str,
    expected: &str,
    mismatches: &mut Vec<SurfaceMismatch>,
) {
    let actual = match read_surface(root.join(relative_file)) {
        Ok(contents) => match serde_json::from_str::<Value>(&contents) {
            Ok(json) => match json.get("version") {
                Some(Value::String(version)) if !version.is_empty() => version.clone(),
                Some(Value::String(_)) | None => missing(),
                Some(_) => invalid(".version is not a string"),
            },
            Err(error) => invalid(&format!("invalid JSON: {error}")),
        },
        Err(actual) => actual,
    };
    record_mismatch(mismatches, expected, actual, relative_file, ".version");
}

fn check_yaml_package_version(
    root: &Path,
    relative_file: &'static str,
    expected: &str,
    mismatches: &mut Vec<SurfaceMismatch>,
) {
    let actual = match read_surface(root.join(relative_file)) {
        Ok(contents) => {
            let values: Vec<_> = contents
                .lines()
                .filter_map(|line| line.strip_prefix("PackageVersion:").map(str::trim))
                .collect();
            match values.as_slice() {
                [] => missing(),
                [""] => missing(),
                [value] => (*value).to_string(),
                _ => invalid("duplicate PackageVersion"),
            }
        }
        Err(actual) => actual,
    };
    record_mismatch(
        mismatches,
        expected,
        actual,
        relative_file,
        "PackageVersion",
    );
}

fn check_installer_basename(root: &Path, version: &str, mismatches: &mut Vec<SurfaceMismatch>) {
    let expected = setup_exe_name(version);
    let actual = match read_surface(root.join(WINGET_INSTALLER)) {
        Ok(contents) => {
            let values: Vec<_> = contents
                .lines()
                .filter_map(|line| {
                    let line = line.trim_start();
                    let line = line.strip_prefix("- ").unwrap_or(line);
                    line.strip_prefix("InstallerUrl:").map(str::trim)
                })
                .collect();
            match values.as_slice() {
                [] => missing(),
                [""] => missing(),
                [value] => installer_basename(value),
                _ => invalid("duplicate InstallerUrl"),
            }
        }
        Err(actual) => actual,
    };
    record_mismatch(
        mismatches,
        &expected,
        actual,
        WINGET_INSTALLER,
        "Installers[].InstallerUrl asset basename",
    );
}

fn installer_basename(value: &str) -> String {
    let unquoted = match matching_unquote(value) {
        Ok(value) if !value.is_empty() => value,
        Ok(_) => return missing(),
        Err(reason) => return invalid(reason),
    };
    let path_end = unquoted.find(['?', '#']).unwrap_or(unquoted.len());
    let path = &unquoted[..path_end];
    match path.rsplit('/').next() {
        Some(basename) if !basename.is_empty() => basename.to_string(),
        _ => invalid("URL has no asset basename"),
    }
}

fn matching_unquote(value: &str) -> Result<&str, &'static str> {
    let bytes = value.as_bytes();
    let Some(first) = bytes.first().copied() else {
        return Ok(value);
    };
    if first != b'\'' && first != b'"' {
        return Ok(value);
    }
    if bytes.len() < 2 || bytes.last().copied() != Some(first) {
        return Err("unmatched quote");
    }
    Ok(&value[1..value.len() - 1])
}

fn read_surface(path: PathBuf) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| match error.kind() {
        io::ErrorKind::NotFound => missing(),
        _ => invalid(&format!("read failed: {error}")),
    })
}

fn record_mismatch(
    mismatches: &mut Vec<SurfaceMismatch>,
    expected: &str,
    actual: String,
    relative_file: &'static str,
    field: &'static str,
) {
    if actual != expected {
        mismatches.push(SurfaceMismatch {
            expected: expected.to_string(),
            actual,
            relative_file,
            field,
        });
    }
}

fn missing() -> String {
    "<missing>".to_string()
}

fn invalid(reason: &str) -> String {
    format!("<invalid: {reason}>")
}

fn one_line(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::setup_exe_name;

    #[test]
    fn setup_executable_name_embeds_only_the_version() {
        assert_eq!(setup_exe_name("0.2.11"), "solstone-setup-0.2.11.exe");
    }
}
