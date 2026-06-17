// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Windows.Graphics.Capture screen source.
//!
//! **Platform tier** — this is where the `windows-rs` quarantine and the only
//! permitted `unsafe` live. The crate's whole job is to implement the pure-tier
//! [`ScreenSource`](observer_model::ScreenSource) trait against WGC; the engine
//! is injected the resulting `dyn ScreenSource` and never sees a `windows` type.
//!
//! Bootstrap state: an API-call-free skeleton so the workspace compiles on the
//! Linux dev box. The real WGC capture loop (frame pool, D3D11 device, dirty-rect
//! scaling) lands in a later wave on the Windows build box. The `windows` crate
//! is declared `[target.'cfg(windows)'.dependencies]`, so it is simply absent
//! off-Windows.

use observer_model::{ScreenSource, SourceError, SourceState};

/// WGC-backed screen source. Skeleton: reports `Inactive` until the real capture
/// path is wired up on the build box.
#[derive(Debug, Default)]
pub struct WgcScreenSource {
    started: bool,
}

impl WgcScreenSource {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ScreenSource for WgcScreenSource {
    fn start(&mut self) -> Result<(), SourceError> {
        // TODO(build box): create the GraphicsCaptureItem, frame pool, and
        // D3D11 device; begin pumping frames. unsafe WinRT/COM lives here.
        self.started = true;
        Ok(())
    }

    fn stop(&mut self) {
        self.started = false;
    }

    fn state(&self) -> SourceState {
        if self.started {
            SourceState::Inactive
        } else {
            SourceState::Inactive
        }
    }
}
