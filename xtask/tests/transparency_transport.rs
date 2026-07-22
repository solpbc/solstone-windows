// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

#[path = "support/transparency.rs"]
mod transparency_support;

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use transparency_support::{DirectoryTransparencyTransport, RecordedTransparencyCall};
use xtask::release_exec::{CommandOutput, CommandRunner, CommandRunnerError};
use xtask::transparency_transport::{
    CurlTransparencyTransport, TransparencyCachePolicy, TransparencyFetchPolicy,
    TransparencyObjectDestination, TransparencyObjectTransport, TransparencyPlane,
    TransparencyS3Credentials, TransparencyTransportError,
};

type RecordedCurlInvocation = (
    PathBuf,
    Vec<String>,
    Option<Vec<u8>>,
    Option<BTreeMap<String, String>>,
);

struct CurlShapeRunner {
    invocation: Mutex<Option<RecordedCurlInvocation>>,
}

impl CurlShapeRunner {
    fn new() -> Self {
        Self {
            invocation: Mutex::new(None),
        }
    }
}

impl CommandRunner for CurlShapeRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        *self.invocation.lock().expect("record curl invocation") = Some((
            program.to_path_buf(),
            args.to_vec(),
            stdin.map(<[u8]>::to_vec),
            env.cloned(),
        ));
        let header_path = argument_value(args, "--dump-header");
        let output_path = argument_value(args, "--output");
        fs::write(
            header_path,
            b"HTTP/1.1 412 Precondition Failed\r\nETag: \"old\"\r\n\r\n",
        )
        .expect("write curl response headers");
        fs::write(output_path, b"existing bytes").expect("write curl response body");
        Ok(CommandOutput {
            status: 22,
            stdout: Vec::new(),
            stderr: b"child detail stays private".to_vec(),
        })
    }
}

#[test]
fn transparency_curl_credentials_reach_only_the_stdin_config() {
    let root = temporary_root("curl-shape");
    let runner = CurlShapeRunner::new();
    let transport = CurlTransparencyTransport::new(
        &runner,
        absolute_program("curl"),
        root.join("scratch"),
        "https://objects.example.invalid".to_owned(),
        "https://public.example.invalid".to_owned(),
        "release-bucket".to_owned(),
        TransparencyS3Credentials::new(
            "shape-access-value".to_owned(),
            "shape-secret-value".to_owned(),
        ),
    )
    .expect("construct curl transport");
    let response = transport
        .create_only_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: "releases/solstone-windows/v/0.2.11/ledger-entry.json".to_owned(),
            },
            b"body\n",
            TransparencyCachePolicy::Immutable,
        )
        .expect("observe conditional response");
    assert_eq!(response.status, 412);
    assert_eq!(response.body, b"existing bytes");

    let (_, args, stdin, env) = runner
        .invocation
        .lock()
        .expect("read curl invocation")
        .clone()
        .expect("curl invocation recorded");
    assert_eq!(&args[..2], ["-K", "-"]);
    assert!(args
        .windows(2)
        .any(|pair| pair == ["--header", "If-None-Match: *"]));
    let argv = args.join(" ");
    assert!(!argv.contains("shape-access-value"));
    assert!(!argv.contains("shape-secret-value"));
    assert_eq!(env, None);
    let stdin = String::from_utf8(stdin.expect("curl config stdin")).expect("ASCII curl config");
    assert_eq!(
        stdin,
        "aws-sigv4 = \"aws:amz:auto:s3\"\nuser = \"shape-access-value:shape-secret-value\"\n"
    );
}

#[test]
fn transparency_directory_transport_enforces_conditional_writes_and_records_statuses() {
    let transport = DirectoryTransparencyTransport::new(temporary_root("directory-fake"));
    let destination = TransparencyObjectDestination {
        plane: TransparencyPlane::S3,
        key: "releases/solstone-windows/latest.json".to_owned(),
    };
    assert_eq!(
        transport
            .create_only_put(&destination, b"first", TransparencyCachePolicy::NoCache)
            .expect("first create")
            .status,
        201
    );
    assert_eq!(
        transport
            .create_only_put(&destination, b"second", TransparencyCachePolicy::NoCache)
            .expect("conflicting create")
            .status,
        412
    );
    let fetched = transport
        .get(&destination, TransparencyFetchPolicy::Bypass)
        .expect("fetch object");
    assert_eq!(fetched.status, 200);
    assert_eq!(fetched.body, b"first");
    let wrong = transport
        .mutable_put(
            &destination,
            b"third",
            TransparencyCachePolicy::NoCache,
            Some("\"not-current\""),
        )
        .expect("conditional update");
    assert_eq!(wrong.status, 412);
    assert!(matches!(
        transport.calls().last(),
        Some(RecordedTransparencyCall::MutablePut(_, _, _, _))
    ));

    transport.inject(Err(TransparencyTransportError::InvocationFailed));
    assert_eq!(
        transport.get(&destination, TransparencyFetchPolicy::Bypass),
        Err(TransparencyTransportError::InvocationFailed)
    );

    transport.inject(Ok(xtask::transparency_transport::ObservedHttpResponse {
        status: 403,
        body: b"denied body despite a successful child exit".to_vec(),
        etag: None,
    }));
    let denied = transport
        .get(&destination, TransparencyFetchPolicy::Bypass)
        .expect("observe HTTP denial independently of process status");
    assert_eq!(denied.status, 403);
    assert_eq!(denied.body, b"denied body despite a successful child exit");
}

fn argument_value<'a>(args: &'a [String], name: &str) -> &'a str {
    let index = args
        .iter()
        .position(|argument| argument == name)
        .expect("curl argument present");
    &args[index + 1]
}

fn temporary_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "solstone-transparency-{label}-{}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).expect("create temporary root");
    root
}

#[cfg(not(windows))]
fn absolute_program(name: &str) -> PathBuf {
    PathBuf::from(format!("/fake-tools/{name}"))
}

#[cfg(windows)]
fn absolute_program(name: &str) -> PathBuf {
    PathBuf::from(format!(r"C:\fake-tools\{name}.exe"))
}
