// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Rotating file writer and tracing MakeWriter adapter.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

/// Append-oriented rotating file writer.
pub struct RotatingFileWriter {
    dir: PathBuf,
    prefix: &'static str,
    max_bytes: u64,
    max_files: usize,
    file: Option<File>,
    size: u64,
}

impl RotatingFileWriter {
    /// Open the active file in append/create mode and initialize size tracking.
    pub fn new(
        dir: impl AsRef<Path>,
        prefix: &'static str,
        max_bytes: u64,
        max_files: usize,
    ) -> io::Result<Self> {
        let dir = dir.as_ref().to_path_buf();
        let active = active_path(&dir, prefix);
        let file = OpenOptions::new().create(true).append(true).open(&active)?;
        let size = file.metadata()?.len();
        Ok(Self {
            dir,
            prefix,
            max_bytes,
            max_files: max_files.max(1),
            file: Some(file),
            size,
        })
    }

    fn active_path(&self) -> PathBuf {
        active_path(&self.dir, self.prefix)
    }

    fn rolled_path(&self, index: usize) -> PathBuf {
        self.dir.join(format!("{}.{}.log", self.prefix, index))
    }

    fn source_path_for_index(&self, index: usize) -> PathBuf {
        if index == 0 {
            self.active_path()
        } else {
            self.rolled_path(index)
        }
    }

    fn ensure_file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let active = self.active_path();
            let file = OpenOptions::new().create(true).append(true).open(&active)?;
            self.size = file.metadata()?.len();
            self.file = Some(file);
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file unavailable"))
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut file) = self.file.take() {
            file.flush()?;
        }

        if self.max_files <= 1 {
            remove_if_present(self.active_path())?;
            self.file = Some(File::create(self.active_path())?);
            self.size = 0;
            return Ok(());
        }

        remove_if_present(self.rolled_path(self.max_files - 1))?;
        for i in (1..=self.max_files - 1).rev() {
            let src = self.source_path_for_index(i - 1);
            let dst = self.rolled_path(i);
            rename_if_present(src, dst)?;
        }
        self.file = Some(File::create(self.active_path())?);
        self.size = 0;
        Ok(())
    }
}

impl Write for RotatingFileWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.size > 0 && self.size.saturating_add(buf.len() as u64) > self.max_bytes {
            self.rotate()?;
        }
        let n = self.ensure_file()?.write(buf)?;
        self.size = self.size.saturating_add(n as u64);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.ensure_file()?.flush()
    }
}

fn active_path(dir: &Path, prefix: &str) -> PathBuf {
    dir.join(format!("{prefix}.log"))
}

fn remove_if_present(path: PathBuf) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

fn rename_if_present(src: PathBuf, dst: PathBuf) -> io::Result<()> {
    match std::fs::rename(src, dst) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

/// `tracing-subscriber` writer factory over the rotating writer.
#[derive(Clone)]
pub struct LogMakeWriter(Arc<Mutex<RotatingFileWriter>>);

impl LogMakeWriter {
    pub fn new(writer: Arc<Mutex<RotatingFileWriter>>) -> Self {
        Self(writer)
    }
}

impl<'a> MakeWriter<'a> for LogMakeWriter {
    type Writer = LogWriterGuard<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        let guard = self.0.lock().unwrap_or_else(|error| error.into_inner());
        LogWriterGuard { guard }
    }
}

/// Locked writer guard returned by [`LogMakeWriter`].
pub struct LogWriterGuard<'a> {
    guard: MutexGuard<'a, RotatingFileWriter>,
}

impl Write for LogWriterGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.guard.flush()
    }
}
