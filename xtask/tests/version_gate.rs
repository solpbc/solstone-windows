// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#![cfg(unix)]

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering::Relaxed};

use serde_json::json;

const VERSION: &str = "0.2.11";
const TAURI_CONFIG: &str = "src-tauri/tauri.conf.json";
const WINGET_VERSION: &str = "packaging/winget/solpbc.Solstone.yaml";
const WINGET_INSTALLER: &str = "packaging/winget/solpbc.Solstone.installer.yaml";
const WINGET_LOCALE: &str = "packaging/winget/solpbc.Solstone.locale.en-US.yaml";
const SCOOP_MANIFEST: &str = "packaging/scoop/solstone.json";

static NEXT_TEMP: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
    cargo: PathBuf,
    metadata: PathBuf,
    argv_witness: PathBuf,
    cwd_witness: PathBuf,
    offline_witness: PathBuf,
}

impl Fixture {
    fn good() -> Self {
        let root = std::env::temp_dir().join(format!(
            "solstone-version-gate-{}-{}",
            std::process::id(),
            NEXT_TEMP.fetch_add(1, Relaxed)
        ));
        fs::create_dir(&root).expect("create fixture root");

        let fixture = Self {
            cargo: root.join("fake-cargo"),
            metadata: root.join("metadata.json"),
            argv_witness: root.join("cargo-argv.txt"),
            cwd_witness: root.join("cargo-cwd.txt"),
            offline_witness: root.join("cargo-offline.txt"),
            root,
        };
        fixture.write(
            TAURI_CONFIG,
            &json!({"productName": "solstone", "version": VERSION}).to_string(),
        );
        fixture.write(
            WINGET_VERSION,
            &format!("PackageIdentifier: solpbc.Solstone\nPackageVersion: {VERSION}\n"),
        );
        fixture.write(
            WINGET_INSTALLER,
            &format!(
                "PackageIdentifier: solpbc.Solstone\nPackageVersion: {VERSION}\nInstallers:\n  - Architecture: x64\n    InstallerUrl: 'https://fixtures.invalid/unrelated/v999/solstone-setup-{VERSION}.exe'\n    InstallerSha256: deliberately-not-a-version\n"
            ),
        );
        fixture.write(
            WINGET_LOCALE,
            &format!("PackageIdentifier: solpbc.Solstone\nPackageVersion: {VERSION}\n"),
        );
        fixture.write(
            SCOOP_MANIFEST,
            &json!({
                "version": VERSION,
                "architecture": {
                    "64bit": {
                        "url": "https://fixtures.invalid/releases/v999/Solstone-win-Portable.zip"
                    }
                }
            })
            .to_string(),
        );
        fixture.write(
            "ui/package.json",
            r#"{"version":"99.99.99","private":true}"#,
        );
        for stable_name in [
            "RELEASES",
            "releases.win.json",
            "assets.win.json",
            "Solstone-win-Portable.zip",
        ] {
            fixture.write(stable_name, "ignored-version-99.99.99\n");
        }
        fixture.set_metadata(json!({
            "packages": [{"name": "solstone-windows-app", "version": VERSION}]
        }));
        fixture.write_fake_cargo();
        fixture
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).expect("create fixture parent");
        }
        fs::write(path, contents).expect("write fixture surface");
    }

    fn set_metadata(&self, value: serde_json::Value) {
        fs::write(&self.metadata, value.to_string()).expect("write metadata");
    }

    fn set_raw_metadata(&self, value: &str) {
        fs::write(&self.metadata, value).expect("write raw metadata");
    }

    fn write_fake_cargo(&self) {
        fs::write(
            &self.cargo,
            r#"#!/bin/sh
set -eu
printf '%s\n' "$@" > "$FAKE_CARGO_ARGV"
pwd -P > "$FAKE_CARGO_CWD"
printf '%s\n' "${CARGO_NET_OFFLINE-}" > "$FAKE_CARGO_OFFLINE"
if [ "${FAKE_CARGO_EXIT-0}" -ne 0 ]; then
  echo "fake metadata failure" >&2
  exit "$FAKE_CARGO_EXIT"
fi
cat "$FAKE_CARGO_METADATA"
"#,
        )
        .expect("write fake cargo");
        let mut permissions = fs::metadata(&self.cargo)
            .expect("read fake cargo metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&self.cargo, permissions).expect("make fake cargo executable");
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_xtask"));
        command
            .args(["version-gate", "--root"])
            .arg(&self.root)
            .env("SOLSTONE_VERSION_GATE_CARGO", &self.cargo)
            .env("CARGO", self.root.join("poisoned-cargo-must-not-run"))
            .env("FAKE_CARGO_METADATA", &self.metadata)
            .env("FAKE_CARGO_ARGV", &self.argv_witness)
            .env("FAKE_CARGO_CWD", &self.cwd_witness)
            .env("FAKE_CARGO_OFFLINE", &self.offline_witness);
        command
    }

    fn run(&self) -> Output {
        self.command().output().expect("run version-gate")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove fixture root");
    }
}

#[test]
fn dedicated_cargo_override_records_exact_metadata_invocation_and_success_output() {
    let fixture = Fixture::good();
    let output = fixture.run();

    assert_success(&output);
    assert_eq!(stdout(&output), format!("{VERSION}\n"));
    assert_eq!(stderr(&output), "");
    assert_eq!(
        read(&fixture.argv_witness),
        "metadata\n--no-deps\n--format-version\n1\n--locked\n"
    );
    assert_eq!(
        PathBuf::from(read(&fixture.cwd_witness).trim()),
        fs::canonicalize(&fixture.root).expect("canonical fixture root")
    );
    assert_eq!(read(&fixture.offline_witness), "true\n");
}

#[test]
fn metadata_spawn_failure_fails_as_an_authority_error() {
    let fixture = Fixture::good();
    let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["version-gate", "--root"])
        .arg(&fixture.root)
        .env(
            "SOLSTONE_VERSION_GATE_CARGO",
            fixture.root.join("missing-cargo"),
        )
        .output()
        .expect("run version-gate");

    assert_failure(&output);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with("ERROR: version-gate: failed to run cargo metadata: "));
    assert_eq!(stderr(&output).lines().count(), 1);
}

#[test]
fn metadata_nonzero_exit_fails_as_one_authority_error() {
    let fixture = Fixture::good();
    let output = fixture
        .command()
        .env("FAKE_CARGO_EXIT", "23")
        .output()
        .expect("run version-gate");

    assert_failure(&output);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        "ERROR: version-gate: cargo metadata exited 23: fake metadata failure\n"
    );
}

#[test]
fn invalid_metadata_json_fails_as_one_authority_error() {
    let fixture = Fixture::good();
    fixture.set_raw_metadata("not json");

    let output = fixture.run();
    assert_failure(&output);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with("ERROR: version-gate: invalid cargo metadata JSON: "));
    assert_eq!(stderr(&output).lines().count(), 1);
}

#[test]
fn metadata_requires_exactly_one_app_package() {
    for (packages, expected_count) in [
        (json!([{"name": "another-package", "version": VERSION}]), 0),
        (
            json!([
                {"name": "solstone-windows-app", "version": VERSION},
                {"name": "solstone-windows-app", "version": VERSION}
            ]),
            2,
        ),
    ] {
        let fixture = Fixture::good();
        fixture.set_metadata(json!({"packages": packages}));

        let output = fixture.run();
        assert_failure(&output);
        assert_eq!(stdout(&output), "");
        assert_eq!(
            stderr(&output),
            format!(
                "ERROR: version-gate: cargo metadata matched {expected_count} packages named solstone-windows-app\n"
            )
        );
    }
}

#[test]
fn metadata_requires_a_nonempty_string_app_version() {
    for package in [
        json!({"name": "solstone-windows-app"}),
        json!({"name": "solstone-windows-app", "version": ""}),
        json!({"name": "solstone-windows-app", "version": 211}),
    ] {
        let fixture = Fixture::good();
        fixture.set_metadata(json!({"packages": [package]}));

        let output = fixture.run();
        assert_failure(&output);
        assert_eq!(stdout(&output), "");
        assert_eq!(
            stderr(&output),
            "ERROR: version-gate: cargo metadata package solstone-windows-app omitted a non-empty version\n"
        );
    }
}

#[test]
fn json_surface_drift_names_the_exact_file_and_field() {
    for path in [TAURI_CONFIG, SCOOP_MANIFEST] {
        let fixture = Fixture::good();
        fixture.write(path, r#"{"version":"0.2.10"}"#);

        let output = fixture.run();
        assert_surface_failure(&output, &diagnostic(VERSION, "0.2.10", path, ".version"));
    }
}

#[test]
fn every_winget_package_version_drift_names_the_exact_manifest() {
    for path in [WINGET_VERSION, WINGET_INSTALLER, WINGET_LOCALE] {
        let fixture = Fixture::good();
        if path == WINGET_INSTALLER {
            fixture.write(
                path,
                &format!(
                    "PackageVersion: 0.2.10\nInstallers:\n  - InstallerUrl: https://fixtures.invalid/solstone-setup-{VERSION}.exe\n"
                ),
            );
        } else {
            fixture.write(path, "PackageVersion: 0.2.10\n");
        }

        let output = fixture.run();
        assert_surface_failure(
            &output,
            &diagnostic(VERSION, "0.2.10", path, "PackageVersion"),
        );
    }
}

#[test]
fn missing_duplicate_and_malformed_fields_fail_closed() {
    let fixture = Fixture::good();
    fixture.write(TAURI_CONFIG, r#"{"productName":"solstone"}"#);
    assert_surface_failure(
        &fixture.run(),
        &diagnostic(VERSION, "<missing>", TAURI_CONFIG, ".version"),
    );

    let fixture = Fixture::good();
    fixture.write(SCOOP_MANIFEST, "{not-json");
    let output = fixture.run();
    assert_failure(&output);
    assert_eq!(stdout(&output), "");
    assert!(stderr(&output).starts_with(&format!(
        "ERROR: version-gate mismatch: expected {VERSION} from cargo metadata solstone-windows-app, actual <invalid: invalid JSON: "
    )));
    assert!(stderr(&output).ends_with(&format!("> at {SCOOP_MANIFEST} field .version.\n")));

    let fixture = Fixture::good();
    fixture.write(
        WINGET_VERSION,
        &format!("PackageVersion: {VERSION}\nPackageVersion: {VERSION}\n"),
    );
    assert_surface_failure(
        &fixture.run(),
        &diagnostic(
            VERSION,
            "<invalid: duplicate PackageVersion>",
            WINGET_VERSION,
            "PackageVersion",
        ),
    );

    let fixture = Fixture::good();
    fixture.write(
        WINGET_INSTALLER,
        &format!("PackageVersion: {VERSION}\nInstallers:\n  - Architecture: x64\n"),
    );
    assert_surface_failure(
        &fixture.run(),
        &diagnostic(
            &format!("solstone-setup-{VERSION}.exe"),
            "<missing>",
            WINGET_INSTALLER,
            "Installers[].InstallerUrl asset basename",
        ),
    );

    let fixture = Fixture::good();
    fixture.write(
        WINGET_INSTALLER,
        &format!(
            "PackageVersion: {VERSION}\nInstallers:\n  - InstallerUrl: 'https://fixtures.invalid/solstone-setup-{VERSION}.exe\n"
        ),
    );
    assert_surface_failure(
        &fixture.run(),
        &diagnostic(
            &format!("solstone-setup-{VERSION}.exe"),
            "<invalid: unmatched quote>",
            WINGET_INSTALLER,
            "Installers[].InstallerUrl asset basename",
        ),
    );

    let fixture = Fixture::good();
    fixture.write(
        WINGET_INSTALLER,
        &format!(
            "PackageVersion: {VERSION}\nInstallers:\n  - InstallerUrl: https://fixtures.invalid/one.exe\n  - InstallerUrl: https://fixtures.invalid/two.exe\n"
        ),
    );
    assert_surface_failure(
        &fixture.run(),
        &diagnostic(
            &format!("solstone-setup-{VERSION}.exe"),
            "<invalid: duplicate InstallerUrl>",
            WINGET_INSTALLER,
            "Installers[].InstallerUrl asset basename",
        ),
    );
}

#[test]
fn multi_surface_drift_reports_every_disagreement_in_surface_order() {
    let fixture = Fixture::good();
    fixture.write(TAURI_CONFIG, r#"{"version":"0.2.10"}"#);
    fixture.write(WINGET_VERSION, "PackageVersion: 0.2.9\n");
    fixture.write(SCOOP_MANIFEST, r#"{"version":"0.2.8"}"#);

    let output = fixture.run();
    assert_failure(&output);
    assert_eq!(stdout(&output), "");
    assert_eq!(
        stderr(&output),
        format!(
            "{}{}{}",
            diagnostic(VERSION, "0.2.10", TAURI_CONFIG, ".version"),
            diagnostic(VERSION, "0.2.9", WINGET_VERSION, "PackageVersion"),
            diagnostic(VERSION, "0.2.8", SCOOP_MANIFEST, ".version")
        )
    );
}

#[test]
fn installer_url_ignores_host_path_tag_query_and_fragment() {
    let fixture = Fixture::good();
    fixture.write(
        WINGET_INSTALLER,
        &format!(
            "PackageVersion: {VERSION}\nInstallers:\n  - InstallerUrl: \"https://different.invalid/not-the-repo/v12345/solstone-setup-{VERSION}.exe?download=1#fragment\"\n"
        ),
    );

    assert_success(&fixture.run());
}

#[test]
fn installer_url_rejects_only_a_wrong_asset_basename() {
    let fixture = Fixture::good();
    fixture.write(
        WINGET_INSTALLER,
        &format!(
            "PackageVersion: {VERSION}\nInstallers:\n  - InstallerUrl: https://fixtures.invalid/v{VERSION}/wrong-name.exe\n"
        ),
    );

    assert_surface_failure(
        &fixture.run(),
        &diagnostic(
            &format!("solstone-setup-{VERSION}.exe"),
            "wrong-name.exe",
            WINGET_INSTALLER,
            "Installers[].InstallerUrl asset basename",
        ),
    );
}

#[test]
fn exempt_versions_hashes_urls_and_stable_names_are_never_checked() {
    let fixture = Fixture::good();
    fixture.write("ui/package.json", r#"{"version":"totally-different"}"#);
    fixture.write(
        SCOOP_MANIFEST,
        &json!({
            "version": VERSION,
            "architecture": {"64bit": {"url": "https://elsewhere.invalid/v0.0.1/wrong.zip"}}
        })
        .to_string(),
    );
    fixture.write(
        WINGET_INSTALLER,
        &format!(
            "PackageVersion: {VERSION}\nInstallerSha256: 0.0.1\nInstallers:\n  - InstallerUrl: https://elsewhere.invalid/arbitrary/solstone-setup-{VERSION}.exe\n"
        ),
    );
    for stable_name in [
        "RELEASES",
        "releases.win.json",
        "assets.win.json",
        "Solstone-win-Portable.zip",
    ] {
        fixture.write(stable_name, "0.0.1\n");
    }

    assert_success(&fixture.run());
}

#[test]
fn unknown_or_incomplete_cli_arguments_exit_two_with_usage() {
    for args in [
        vec!["version-gate", "--unknown"],
        vec!["version-gate", "--root"],
        vec!["version-gate", "--root", ""],
        vec!["version-gate", "--root", "/tmp", "extra"],
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_xtask"))
            .args(args)
            .output()
            .expect("run invalid CLI");
        assert_eq!(output.status.code(), Some(2));
        assert_eq!(stdout(&output), "");
        assert_eq!(
            stderr(&output),
            "usage: cargo xtask version-gate [--root <path>]\n"
        );
    }
}

fn diagnostic(expected: &str, actual: &str, file: &str, field: &str) -> String {
    format!(
        "ERROR: version-gate mismatch: expected {expected} from cargo metadata solstone-windows-app, actual {actual} at {file} field {field}.\n"
    )
}

fn assert_surface_failure(output: &Output, expected_stderr: &str) {
    assert_failure(output);
    assert_eq!(stdout(output), "");
    assert_eq!(stderr(output), expected_stderr);
}

fn assert_success(output: &Output) {
    assert!(
        output.status.success(),
        "expected success; stdout={} stderr={}",
        stdout(output),
        stderr(output)
    );
}

fn assert_failure(output: &Output) {
    assert!(
        !output.status.success(),
        "expected failure; stdout={} stderr={}",
        stdout(output),
        stderr(output)
    );
}

fn stdout(output: &Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("stdout is UTF-8")
}

fn stderr(output: &Output) -> String {
    String::from_utf8(output.stderr.clone()).expect("stderr is UTF-8")
}

fn read(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}
