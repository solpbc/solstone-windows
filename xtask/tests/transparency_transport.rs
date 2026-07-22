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
    TransparencyListDestination, TransparencyObjectDestination, TransparencyObjectTransport,
    TransparencyPlane, TransparencyS3Credentials, TransparencyTransportError,
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
            status: 0,
            stdout: Vec::new(),
            stderr: b"child detail stays private".to_vec(),
        })
    }
}

struct CurlDeniedRunner;

impl CommandRunner for CurlDeniedRunner {
    fn run(
        &self,
        _program: &Path,
        args: &[String],
        _stdin: Option<&[u8]>,
        _env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        fs::write(
            argument_value(args, "--dump-header"),
            b"HTTP/1.1 403 Forbidden\r\n\r\n",
        )
        .expect("write denied response headers");
        fs::write(argument_value(args, "--output"), b"denied").expect("write denied response body");
        Ok(CommandOutput {
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
        })
    }
}

struct CurlLeavesNoResponseRunner;

impl CommandRunner for CurlLeavesNoResponseRunner {
    fn run(
        &self,
        _program: &Path,
        _args: &[String],
        _stdin: Option<&[u8]>,
        _env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        Ok(CommandOutput {
            status: 0,
            stdout: Vec::new(),
            stderr: Vec::new(),
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

    let (program, args, stdin, env) = runner
        .invocation
        .lock()
        .expect("read curl invocation")
        .clone()
        .expect("curl invocation recorded");
    assert_eq!(program, absolute_program("curl"));
    assert_eq!(
        normalized_curl_args(&args),
        [
            "-K",
            "-",
            "--silent",
            "--show-error",
            "--request",
            "PUT",
            "--url",
            "https://objects.example.invalid/release-bucket/releases/solstone-windows/v/0.2.11/ledger-entry.json",
            "--header",
            "If-None-Match: *",
            "--header",
            "Cache-Control: public,max-age=31536000,immutable",
            "--upload-file",
            "<request>",
            "--dump-header",
            "<headers>",
            "--output",
            "<response>",
        ]
    );
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
fn transparency_curl_list_builds_a_url_for_a_trailing_slash_prefix() {
    let root = temporary_root("curl-list-prefix");
    let runner = CurlShapeRunner::new();
    let transport = CurlTransparencyTransport::new(
        &runner,
        absolute_program("curl"),
        root.join("scratch"),
        "https://objects.example.invalid".to_owned(),
        "https://public.example.invalid".to_owned(),
        "release-bucket".to_owned(),
        TransparencyS3Credentials::new("access".to_owned(), "secret".to_owned()),
    )
    .expect("construct curl transport");

    transport
        .list(&TransparencyListDestination {
            prefix: "releases/solstone-windows/v/".to_owned(),
        })
        .expect("trailing-slash prefix builds a list URL");

    let (program, args, stdin, env) = runner
        .invocation
        .lock()
        .expect("read curl invocation")
        .clone()
        .expect("curl invocation recorded");
    assert_eq!(program, absolute_program("curl"));
    assert_eq!(
        normalized_curl_args(&args),
        [
            "-K",
            "-",
            "--silent",
            "--show-error",
            "--request",
            "GET",
            "--url",
            "https://objects.example.invalid/release-bucket?list-type=2&prefix=releases%2Fsolstone-windows%2Fv%2F",
            "--header",
            "Cache-Control: no-cache",
            "--dump-header",
            "<headers>",
            "--output",
            "<response>",
        ]
    );
    assert!(stdin.is_some());
    assert_eq!(env, None);
}

#[test]
fn transparency_curl_get_builds_the_exact_url_and_argument_shape() {
    let root = temporary_root("curl-get-shape");
    let runner = CurlShapeRunner::new();
    let transport = curl_transport(&runner, &root);

    transport
        .get(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: "releases/solstone-windows/latest.json".to_owned(),
            },
            TransparencyFetchPolicy::Bypass,
        )
        .expect("get builds curl arguments");

    let (program, args, stdin, env) = recorded_invocation(&runner);
    assert_eq!(program, absolute_program("curl"));
    assert_eq!(
        normalized_curl_args(&args),
        [
            "-K",
            "-",
            "--silent",
            "--show-error",
            "--request",
            "GET",
            "--url",
            "https://objects.example.invalid/release-bucket/releases/solstone-windows/latest.json",
            "--header",
            "Cache-Control: no-cache",
            "--dump-header",
            "<headers>",
            "--output",
            "<response>",
        ]
    );
    assert!(stdin.is_some());
    assert_eq!(env, None);
}

#[test]
fn transparency_curl_mutable_put_builds_the_exact_url_and_argument_shape() {
    let root = temporary_root("curl-mutable-shape");
    let runner = CurlShapeRunner::new();
    let transport = curl_transport(&runner, &root);

    transport
        .mutable_put(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: "releases/solstone-windows/latest.json".to_owned(),
            },
            b"pointer\n",
            TransparencyCachePolicy::NoCache,
            Some("\"old\""),
        )
        .expect("mutable put builds curl arguments");

    let (program, args, stdin, env) = recorded_invocation(&runner);
    assert_eq!(program, absolute_program("curl"));
    assert_eq!(
        normalized_curl_args(&args),
        [
            "-K",
            "-",
            "--silent",
            "--show-error",
            "--request",
            "PUT",
            "--url",
            "https://objects.example.invalid/release-bucket/releases/solstone-windows/latest.json",
            "--header",
            "Cache-Control: no-cache",
            "--header",
            "If-Match: \"old\"",
            "--upload-file",
            "<request>",
            "--dump-header",
            "<headers>",
            "--output",
            "<response>",
        ]
    );
    assert!(stdin.is_some());
    assert_eq!(env, None);
}

#[test]
fn transparency_curl_list_prefix_validation_table_fails_closed() {
    let root = temporary_root("curl-list-validation");
    let runner = CurlShapeRunner::new();
    let transport = curl_transport(&runner, &root);

    for prefix in [
        "", "/", "//", "a//b", "a//", "./", "../", "a\\b/", "a\nb/", "a\0b/",
    ] {
        assert_eq!(
            transport.list(&TransparencyListDestination {
                prefix: prefix.to_owned(),
            }),
            Err(TransparencyTransportError::InvalidDestination),
            "prefix {prefix:?} must remain invalid"
        );
    }
    assert!(runner
        .invocation
        .lock()
        .expect("read curl invocation")
        .is_none());

    transport
        .list(&TransparencyListDestination {
            prefix: "releases/solstone-windows/v/0.2.11/".to_owned(),
        })
        .expect("version prefix with one trailing slash is valid");
    let (_, args, _, _) = recorded_invocation(&runner);
    assert_eq!(
        argument_value(&args, "--url"),
        "https://objects.example.invalid/release-bucket?list-type=2&prefix=releases%2Fsolstone-windows%2Fv%2F0.2.11%2F"
    );
}

#[test]
fn transparency_curl_observes_http_denial_on_process_success() {
    let root = temporary_root("curl-denied");
    let transport = CurlTransparencyTransport::new(
        &CurlDeniedRunner,
        absolute_program("curl"),
        root.join("scratch"),
        "https://objects.example.invalid".to_owned(),
        "https://public.example.invalid".to_owned(),
        "release-bucket".to_owned(),
        TransparencyS3Credentials::new("access".to_owned(), "secret".to_owned()),
    )
    .expect("construct curl transport");
    let response = transport
        .get(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: "releases/solstone-windows/latest.json".to_owned(),
            },
            TransparencyFetchPolicy::Bypass,
        )
        .expect("observe HTTP denial");
    assert_eq!(response.status, 403);
    assert_eq!(response.body, b"denied");
}

#[test]
fn transparency_curl_never_reuses_preexisting_response_files() {
    let root = temporary_root("curl-stale");
    let scratch = root.join("scratch");
    fs::create_dir_all(&scratch).expect("create scratch");
    fs::write(scratch.join("headers-0"), b"HTTP/1.1 200 OK\r\n\r\n").expect("seed stale headers");
    fs::write(scratch.join("response-0"), b"stale body").expect("seed stale body");
    let transport = CurlTransparencyTransport::new(
        &CurlLeavesNoResponseRunner,
        absolute_program("curl"),
        scratch,
        "https://objects.example.invalid".to_owned(),
        "https://public.example.invalid".to_owned(),
        "release-bucket".to_owned(),
        TransparencyS3Credentials::new("access".to_owned(), "secret".to_owned()),
    )
    .expect("construct curl transport");
    assert_eq!(
        transport.get(
            &TransparencyObjectDestination {
                plane: TransparencyPlane::S3,
                key: "releases/solstone-windows/latest.json".to_owned(),
            },
            TransparencyFetchPolicy::Bypass,
        ),
        Err(TransparencyTransportError::ResponseUnavailable)
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

fn curl_transport<'a>(
    runner: &'a CurlShapeRunner,
    root: &Path,
) -> CurlTransparencyTransport<'a, CurlShapeRunner> {
    CurlTransparencyTransport::new(
        runner,
        absolute_program("curl"),
        root.join("scratch"),
        "https://objects.example.invalid".to_owned(),
        "https://public.example.invalid".to_owned(),
        "release-bucket".to_owned(),
        TransparencyS3Credentials::new("access".to_owned(), "secret".to_owned()),
    )
    .expect("construct curl transport")
}

fn recorded_invocation(runner: &CurlShapeRunner) -> RecordedCurlInvocation {
    runner
        .invocation
        .lock()
        .expect("read curl invocation")
        .clone()
        .expect("curl invocation recorded")
}

fn normalized_curl_args(args: &[String]) -> Vec<&str> {
    let mut normalized: Vec<&str> = Vec::with_capacity(args.len());
    let mut next_path: Option<&'static str> = None;
    for argument in args {
        if let Some(replacement) = next_path.take() {
            normalized.push(replacement);
        } else {
            normalized.push(argument.as_str());
            next_path = match argument.as_str() {
                "--upload-file" => Some("<request>"),
                "--dump-header" => Some("<headers>"),
                "--output" => Some("<response>"),
                _ => None,
            };
        }
    }
    normalized
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
