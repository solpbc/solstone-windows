// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! WASAPI audio sources: render-loopback system audio and eCapture microphone.
//!
//! **Platform tier** — `windows-rs` quarantine; `unsafe` permitted here only.
//! This crate owns the honest [`SourceState::NoInputDevice`] determination: when
//! the machine has no microphone endpoint, the mic source reports
//! `NoInputDevice` (a first-class, non-error state), never a fake "active".
//!
//! Bootstrap state: API-call-free skeletons so the workspace compiles on Linux.
//! The real WASAPI loopback client, eCapture mic client, and endpoint
//! enumeration land in a later wave on the Windows build box.

use observer_model::{MicSource, SourceError, SourceState, SystemAudioSource};

/// WASAPI render-loopback system-audio source.
#[derive(Debug, Default)]
pub struct WasapiSystemAudioSource {
    started: bool,
}

impl WasapiSystemAudioSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SystemAudioSource for WasapiSystemAudioSource {
    fn start(&mut self) -> Result<(), SourceError> {
        // TODO(build box): activate the render endpoint in loopback mode.
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.started = false;
    }

    fn state(&self) -> SourceState {
        SourceState::Inactive
    }
}

/// WASAPI eCapture microphone source. Owns the no-mic case.
#[derive(Debug, Default)]
pub struct WasapiMicSource {
    started: bool,
}

impl WasapiMicSource {
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a microphone capture endpoint exists on this machine. The real
    /// implementation enumerates `eCapture` endpoints; the skeleton conservatively
    /// reports none so the no-device path is the default until wired up.
    fn has_input_device(&self) -> bool {
        // TODO(build box): enumerate eCapture endpoints via IMMDeviceEnumerator.
        false
    }
}

impl MicSource for WasapiMicSource {
    fn start(&mut self) -> Result<(), SourceError> {
        if !self.has_input_device() {
            // Not an error: a machine with no mic is a valid configuration.
            return Ok(());
        }
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.started = false;
    }

    fn state(&self) -> SourceState {
        if !self.has_input_device() {
            SourceState::NoInputDevice
        } else if self.started {
            SourceState::Inactive
        } else {
            SourceState::Inactive
        }
    }
}
