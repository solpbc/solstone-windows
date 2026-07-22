// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Typed, injectable process execution for release tooling.

use std::cell::OnceCell;
use std::collections::BTreeMap;
use std::fmt;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{compiler_fence, Ordering};

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CommandOutput {
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CommandRunnerError {
    ProgramNotAbsolute,
    LaunchFailed,
    StdinWriteFailed,
    WaitFailed,
    UnexpectedInvocation,
    FakeStatePoisoned,
}

impl fmt::Display for CommandRunnerError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::ProgramNotAbsolute => write!(
                formatter,
                "selected command program is not absolute; rerun release-tool preflight and pass its selected executable path"
            ),
            Self::LaunchFailed => write!(
                formatter,
                "selected command could not be launched; restore the preflight-selected executable and retry"
            ),
            Self::StdinWriteFailed => write!(
                formatter,
                "selected command did not accept its complete stdin payload; retry from a new transaction"
            ),
            Self::WaitFailed => write!(
                formatter,
                "selected command could not be observed through completion; retry from a new transaction"
            ),
            Self::UnexpectedInvocation => write!(
                formatter,
                "command invocation did not match the injected action contract; fix the action order or argv and retry"
            ),
            Self::FakeStatePoisoned => write!(
                formatter,
                "command witness state is unavailable; recreate the test runner and retry"
            ),
        }
    }
}

impl std::error::Error for CommandRunnerError {}

pub trait CommandRunner {
    fn record_phase(&self, _phase: &'static str) -> Result<(), CommandRunnerError> {
        Ok(())
    }

    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError>;

    fn run_interactive(
        &self,
        program: &Path,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        self.run(program, args, None, env)
    }
}

const UNIX_PASSPHRASE_READER: &str = "exec 2>/dev/tty || exit 1\nsaved=$(stty -g) || exit 1\ntrap 'stty \"$saved\"' 0 HUP INT TERM\nprintf 'Transparency signing key passphrase: ' >&2\nstty -echo || exit 1\nIFS= read -r passphrase\nread_status=$?\nstty \"$saved\" || exit 1\ntrap - 0 HUP INT TERM\nprintf '\\n' >&2\n[ \"$read_status\" -eq 0 ] || exit 1\n[ -n \"$passphrase\" ] || exit 1\nprintf '%s\\n' \"$passphrase\"";
const WINDOWS_PASSPHRASE_READER: &str = "$secure = Read-Host -Prompt 'Transparency signing key passphrase' -AsSecureString; $bstr = [Runtime.InteropServices.Marshal]::SecureStringToBSTR($secure); $plain = $null; $bytes = $null; try { $plain = [Runtime.InteropServices.Marshal]::PtrToStringBSTR($bstr); if ([String]::IsNullOrEmpty($plain)) { exit 1 }; $bytes = [Text.Encoding]::UTF8.GetBytes($plain); $stdout = [Console]::OpenStandardOutput(); $stdout.Write($bytes, 0, $bytes.Length); $stdout.WriteByte(10); $stdout.Flush() } finally { if ($null -ne $bytes) { [Array]::Clear($bytes, 0, $bytes.Length) }; $plain = $null; [Runtime.InteropServices.Marshal]::ZeroFreeBSTR($bstr) }";

pub(crate) fn build_passphrase_reader_command(
    windows: bool,
    system_root: Option<&str>,
) -> (PathBuf, Vec<String>) {
    if windows {
        let system_root = system_root
            .unwrap_or(r"C:\Windows")
            .trim_end_matches(['\\', '/']);
        (
            PathBuf::from(format!(
                r"{system_root}\System32\WindowsPowerShell\v1.0\powershell.exe"
            )),
            vec![
                "-NoProfile".to_owned(),
                "-Command".to_owned(),
                WINDOWS_PASSPHRASE_READER.to_owned(),
            ],
        )
    } else {
        (
            PathBuf::from("/bin/sh"),
            vec!["-c".to_owned(), UNIX_PASSPHRASE_READER.to_owned()],
        )
    }
}

pub(crate) struct Passphrase {
    bytes: Vec<u8>,
}

impl Passphrase {
    pub(crate) fn from_command_stdout(mut bytes: Vec<u8>) -> Result<Self, ()> {
        if !bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            wipe_bytes(&mut bytes);
            return Err(());
        }
        while bytes
            .last()
            .is_some_and(|byte| matches!(byte, b'\r' | b'\n'))
        {
            bytes.truncate(bytes.len() - 1);
        }
        if bytes.is_empty() || bytes.iter().any(|byte| byte.is_ascii_control()) {
            wipe_bytes(&mut bytes);
            return Err(());
        }
        bytes.push(b'\n');
        Ok(Self { bytes })
    }

    pub(crate) fn stdin(&self) -> &[u8] {
        &self.bytes
    }

    pub(crate) fn wipe(&mut self) {
        wipe_bytes(&mut self.bytes);
    }
}

impl Drop for Passphrase {
    fn drop(&mut self) {
        self.wipe();
    }
}

#[derive(Clone, Copy)]
enum PassphraseAcquisitionError {
    Invocation,
    Rejected,
    InvalidOutput,
}

pub(crate) struct PassphraseCache<'a> {
    runner: &'a dyn CommandRunner,
    cell: OnceCell<Result<Passphrase, PassphraseAcquisitionError>>,
}

impl<'a> PassphraseCache<'a> {
    pub(crate) fn new(runner: &'a dyn CommandRunner) -> Self {
        Self {
            runner,
            cell: OnceCell::new(),
        }
    }

    pub(crate) fn get(&self) -> Result<&Passphrase, ()> {
        self.cell
            .get_or_init(|| acquire_passphrase(self.runner))
            .as_ref()
            .map_err(|_| ())
    }

    pub(crate) fn runner(&self) -> &dyn CommandRunner {
        self.runner
    }
}

fn acquire_passphrase(
    runner: &dyn CommandRunner,
) -> Result<Passphrase, PassphraseAcquisitionError> {
    let (program, args) = production_passphrase_reader_command();
    let mut output = runner
        .run_interactive(&program, &args, None)
        .map_err(|_| PassphraseAcquisitionError::Invocation)?;
    if output.status != 0 {
        wipe_bytes(&mut output.stdout);
        return Err(PassphraseAcquisitionError::Rejected);
    }
    Passphrase::from_command_stdout(output.stdout)
        .map_err(|()| PassphraseAcquisitionError::InvalidOutput)
}

fn wipe_bytes(bytes: &mut [u8]) {
    for byte in bytes {
        *byte = 0;
    }
    compiler_fence(Ordering::SeqCst);
}

#[cfg(unix)]
fn production_passphrase_reader_command() -> (PathBuf, Vec<String>) {
    build_passphrase_reader_command(false, None)
}

#[cfg(windows)]
fn production_passphrase_reader_command() -> (PathBuf, Vec<String>) {
    // SystemRoot is an OS-standard location, not an operator configuration variable.
    let system_root = std::env::var("SystemRoot").ok();
    build_passphrase_reader_command(true, system_root.as_deref())
}

#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessCommandRunner;

/// Process runner that inherits the ambient environment except for named values.
#[derive(Clone, Copy, Debug)]
pub struct RemovedEnvironmentProcessCommandRunner<'a> {
    removed: &'a [&'a str],
}

impl<'a> RemovedEnvironmentProcessCommandRunner<'a> {
    pub fn new(removed: &'a [&'a str]) -> Self {
        Self { removed }
    }
}

impl CommandRunner for ProcessCommandRunner {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        run_process(program, args, stdin, env, &[], false)
    }

    fn run_interactive(
        &self,
        program: &Path,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        run_process(program, args, None, env, &[], true)
    }
}

impl CommandRunner for RemovedEnvironmentProcessCommandRunner<'_> {
    fn run(
        &self,
        program: &Path,
        args: &[String],
        stdin: Option<&[u8]>,
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        run_process(program, args, stdin, env, self.removed, false)
    }

    fn run_interactive(
        &self,
        program: &Path,
        args: &[String],
        env: Option<&BTreeMap<String, String>>,
    ) -> Result<CommandOutput, CommandRunnerError> {
        run_process(program, args, None, env, self.removed, true)
    }
}

fn run_process(
    program: &Path,
    args: &[String],
    stdin: Option<&[u8]>,
    env: Option<&BTreeMap<String, String>>,
    removed_environment: &[&str],
    inherit_stdin: bool,
) -> Result<CommandOutput, CommandRunnerError> {
    if !program.is_absolute() {
        return Err(CommandRunnerError::ProgramNotAbsolute);
    }

    let mut command = Command::new(program);
    command
        .args(args)
        .stdin(if inherit_stdin {
            Stdio::inherit()
        } else if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for name in removed_environment {
        command.env_remove(name);
    }
    if let Some(env) = env {
        command.envs(env);
    }

    let mut child = command
        .spawn()
        .map_err(|_| CommandRunnerError::LaunchFailed)?;
    if let Some(bytes) = stdin {
        child
            .stdin
            .take()
            .ok_or(CommandRunnerError::StdinWriteFailed)?
            .write_all(bytes)
            .map_err(|_| CommandRunnerError::StdinWriteFailed)?;
    }
    let output = child
        .wait_with_output()
        .map_err(|_| CommandRunnerError::WaitFailed)?;
    Ok(CommandOutput {
        status: output.status.code().unwrap_or(-1),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

pub mod test_support {
    use std::collections::{BTreeMap, VecDeque};
    use std::path::{Path, PathBuf};
    use std::sync::Mutex;

    use super::{CommandOutput, CommandRunner, CommandRunnerError};

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct CommandInvocation {
        pub program: PathBuf,
        pub args: Vec<String>,
        pub stdin: Option<Vec<u8>>,
        pub env: Option<BTreeMap<String, String>>,
    }

    #[derive(Clone, Debug, Eq, PartialEq)]
    pub struct FakeCommand {
        pub invocation: CommandInvocation,
        pub result: Result<CommandOutput, CommandRunnerError>,
    }

    impl FakeCommand {
        pub fn output(program: PathBuf, args: Vec<String>, output: CommandOutput) -> Self {
            Self {
                invocation: CommandInvocation {
                    program,
                    args,
                    stdin: None,
                    env: None,
                },
                result: Ok(output),
            }
        }
    }

    #[derive(Debug)]
    pub struct FakeCommandRunner {
        expected: Mutex<VecDeque<FakeCommand>>,
        witness: Mutex<Vec<CommandInvocation>>,
    }

    impl FakeCommandRunner {
        pub fn new(expected: Vec<FakeCommand>) -> Self {
            Self {
                expected: Mutex::new(expected.into()),
                witness: Mutex::new(Vec::new()),
            }
        }

        pub fn witness(&self) -> Result<Vec<CommandInvocation>, CommandRunnerError> {
            self.witness
                .lock()
                .map(|witness| witness.clone())
                .map_err(|_| CommandRunnerError::FakeStatePoisoned)
        }

        pub fn remaining(&self) -> Result<usize, CommandRunnerError> {
            self.expected
                .lock()
                .map(|expected| expected.len())
                .map_err(|_| CommandRunnerError::FakeStatePoisoned)
        }
    }

    impl CommandRunner for FakeCommandRunner {
        fn run(
            &self,
            program: &Path,
            args: &[String],
            stdin: Option<&[u8]>,
            env: Option<&BTreeMap<String, String>>,
        ) -> Result<CommandOutput, CommandRunnerError> {
            if !program.is_absolute() {
                return Err(CommandRunnerError::ProgramNotAbsolute);
            }
            let invocation = CommandInvocation {
                program: program.to_path_buf(),
                args: args.to_vec(),
                stdin: stdin.map(<[u8]>::to_vec),
                env: env.cloned(),
            };
            self.witness
                .lock()
                .map_err(|_| CommandRunnerError::FakeStatePoisoned)?
                .push(invocation.clone());
            let expected = self
                .expected
                .lock()
                .map_err(|_| CommandRunnerError::FakeStatePoisoned)?
                .pop_front()
                .ok_or(CommandRunnerError::UnexpectedInvocation)?;
            if expected.invocation != invocation {
                return Err(CommandRunnerError::UnexpectedInvocation);
            }
            expected.result
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_exec::test_support::{CommandInvocation, FakeCommand, FakeCommandRunner};
    use std::path::PathBuf;

    #[cfg(not(windows))]
    const SELECTED_TOOL: &str = "/selected/tool";
    #[cfg(windows)]
    const SELECTED_TOOL: &str = r"C:\selected\tool";

    #[test]
    fn fake_matches_full_typed_invocation_and_records_order() {
        let program = PathBuf::from(SELECTED_TOOL);
        let args = vec!["first".to_owned(), "second".to_owned()];
        let env = BTreeMap::from([("PATH".to_owned(), "isolated".to_owned())]);
        let invocation = CommandInvocation {
            program: program.clone(),
            args: args.clone(),
            stdin: Some(b"request".to_vec()),
            env: Some(env.clone()),
        };
        let output = CommandOutput {
            status: 7,
            stdout: b"out".to_vec(),
            stderr: b"err".to_vec(),
        };
        let runner = FakeCommandRunner::new(vec![FakeCommand {
            invocation: invocation.clone(),
            result: Ok(output.clone()),
        }]);

        assert_eq!(
            runner
                .run(&program, &args, Some(b"request"), Some(&env))
                .expect("run expected fake command"),
            output
        );
        assert_eq!(runner.witness().expect("read witness"), vec![invocation]);
        assert_eq!(runner.remaining().expect("read remaining count"), 0);
    }

    #[test]
    fn fake_refuses_relative_programs_and_unexpected_argv() {
        let runner = FakeCommandRunner::new(Vec::new());
        assert_eq!(
            runner
                .run(Path::new("tool"), &[], None, None)
                .expect_err("relative program must fail"),
            CommandRunnerError::ProgramNotAbsolute
        );

        let runner = FakeCommandRunner::new(vec![FakeCommand::output(
            PathBuf::from(SELECTED_TOOL),
            vec!["expected".to_owned()],
            CommandOutput {
                status: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            },
        )]);
        assert_eq!(
            runner
                .run(
                    Path::new(SELECTED_TOOL),
                    &["unexpected".to_owned()],
                    None,
                    None
                )
                .expect_err("argv drift must fail"),
            CommandRunnerError::UnexpectedInvocation
        );
    }

    #[test]
    fn passphrase_reader_command_shapes_are_stable_on_both_platforms() {
        let (unix_program, unix_args) = build_passphrase_reader_command(false, None);
        assert_eq!(unix_program, PathBuf::from("/bin/sh"));
        assert_eq!(unix_args, ["-c", UNIX_PASSPHRASE_READER]);

        let (windows_program, windows_args) =
            build_passphrase_reader_command(true, Some(r"D:\Windows"));
        assert_eq!(
            windows_program,
            PathBuf::from(r"D:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
        assert_eq!(
            windows_args,
            ["-NoProfile", "-Command", WINDOWS_PASSPHRASE_READER]
        );

        let (fallback_program, _) = build_passphrase_reader_command(true, None);
        assert_eq!(
            fallback_program,
            PathBuf::from(r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe")
        );
    }

    #[test]
    fn passphrase_normalization_reuses_stdout_storage_and_wipes_in_place() {
        let stdout = b"synthetic secret\r\n".to_vec();
        let original_pointer = stdout.as_ptr();
        let mut passphrase =
            Passphrase::from_command_stdout(stdout).expect("normalize reader output");
        assert_eq!(passphrase.stdin(), b"synthetic secret\n");
        assert_eq!(passphrase.stdin().as_ptr(), original_pointer);

        passphrase.wipe();
        assert!(passphrase.stdin().iter().all(|byte| *byte == 0));
        assert!(Passphrase::from_command_stdout(Vec::new()).is_err());
        assert!(Passphrase::from_command_stdout(b"missing newline".to_vec()).is_err());
        assert!(Passphrase::from_command_stdout(b"\r\n".to_vec()).is_err());
        assert!(Passphrase::from_command_stdout(b"embedded\0byte\n".to_vec()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn transparency_redacted_process_runner_removes_publisher_credentials() {
        const CHILD_MARKER: &str = "SOLSTONE_REDACTED_RUNNER_CHILD";
        const SECRET_NAME: &str = "TRANSPARENCY_S3_SECRET_ACCESS_KEY";
        const SECRET_VALUE: &str = "credential-must-not-reach-child";
        const ACCESS_NAME: &str = "TRANSPARENCY_S3_ACCESS_KEY_ID";
        const ACCESS_VALUE: &str = "access-id-must-not-reach-child";
        if std::env::var_os(CHILD_MARKER).is_none() {
            let output = std::process::Command::new(std::env::current_exe().expect("test binary"))
                .args([
                    "--exact",
                    "release_exec::tests::transparency_redacted_process_runner_removes_publisher_credentials",
                ])
                .env(CHILD_MARKER, "1")
                .env(SECRET_NAME, SECRET_VALUE)
                .env(ACCESS_NAME, ACCESS_VALUE)
                .output()
                .expect("run credential-bearing test child");
            assert!(output.status.success());
            assert!(!output
                .stdout
                .windows(SECRET_VALUE.len())
                .any(|window| window == SECRET_VALUE.as_bytes()));
            assert!(!output
                .stderr
                .windows(SECRET_VALUE.len())
                .any(|window| window == SECRET_VALUE.as_bytes()));
            assert!(!output
                .stdout
                .windows(ACCESS_VALUE.len())
                .any(|window| window == ACCESS_VALUE.as_bytes()));
            assert!(!output
                .stderr
                .windows(ACCESS_VALUE.len())
                .any(|window| window == ACCESS_VALUE.as_bytes()));
            return;
        }
        let program = [Path::new("/usr/bin/env"), Path::new("/bin/env")]
            .into_iter()
            .find(|candidate| candidate.is_file())
            .expect("env executable");
        let runner = RemovedEnvironmentProcessCommandRunner::new(&[ACCESS_NAME, SECRET_NAME]);
        let output = runner
            .run(program, &[], None, None)
            .expect("run with publisher credentials removed");
        assert_eq!(output.status, 0);
        assert!(!output
            .stdout
            .windows(SECRET_VALUE.len())
            .any(|window| window == SECRET_VALUE.as_bytes()));
        assert!(!output
            .stdout
            .windows(ACCESS_VALUE.len())
            .any(|window| window == ACCESS_VALUE.as_bytes()));
        assert!(String::from_utf8_lossy(&output.stdout).contains(CHILD_MARKER));
        let interactive = runner
            .run_interactive(program, &[], None)
            .expect("run interactive command with publisher credentials removed");
        assert_eq!(interactive.status, 0);
        assert!(!interactive
            .stdout
            .windows(SECRET_VALUE.len())
            .any(|window| window == SECRET_VALUE.as_bytes()));
        assert!(!interactive
            .stdout
            .windows(ACCESS_VALUE.len())
            .any(|window| window == ACCESS_VALUE.as_bytes()));
    }
}
