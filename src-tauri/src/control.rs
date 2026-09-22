// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use tokio::io::AsyncReadExt;

pub const CONTROL_PORT: u16 = 49248;

const OPEN_JOURNAL_VERB: &[u8] = b"open-journal\n";
const SURFACE_VERB: &[u8] = b"surface-settings\n";
const SURFACE_ABOUT_VERB: &[u8] = b"surface-about\n";

pub fn signal_surface() -> bool {
    signal(SURFACE_VERB)
}

pub fn signal_open_journal() -> bool {
    signal(OPEN_JOURNAL_VERB)
}

/// Surface About on an already-running instance. Without this, `--open-view
/// about` against a live app fell through to the surface verb and opened
/// Settings, so the flag silently did the wrong thing rather than failing.
pub fn signal_surface_about() -> bool {
    signal(SURFACE_ABOUT_VERB)
}

fn signal(verb: &[u8]) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], CONTROL_PORT));
    let mut stream = match TcpStream::connect_timeout(&addr, Duration::from_millis(500)) {
        Ok(stream) => stream,
        Err(_) => return false,
    };
    let timeout = Some(Duration::from_secs(2));
    if stream.set_read_timeout(timeout).is_err() {
        return false;
    }
    if stream.set_write_timeout(timeout).is_err() {
        return false;
    }

    stream.write_all(verb).is_ok()
}

pub async fn serve(app: tauri::AppHandle, listener: tokio::net::TcpListener) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => {
                tokio::time::sleep(Duration::from_millis(100)).await;
                continue;
            }
        };
        let app = app.clone();
        tokio::spawn(async move {
            handle_connection(app, stream).await;
        });
    }
}

async fn handle_connection(app: tauri::AppHandle, mut stream: tokio::net::TcpStream) {
    let mut buf = [0_u8; 64];
    let read = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut buf)).await;
    let n = match read {
        Ok(Ok(n)) => n,
        _ => return,
    };
    if buf[..n].starts_with(OPEN_JOURNAL_VERB) {
        tokio::spawn(async move {
            if let Err(error) = crate::windows::open_journal(&app).await {
                tracing::warn!(
                    target: "window",
                    label = "journal",
                    error = error.token(),
                    "open-journal failed"
                );
            }
        });
    } else if buf[..n].starts_with(SURFACE_ABOUT_VERB) {
        std::thread::spawn(move || {
            let _ = crate::windows::open_about(&app);
        });
    } else if buf[..n].starts_with(SURFACE_VERB) {
        std::thread::spawn(move || {
            let _ = crate::windows::open_settings(&app);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The two surface verbs must not prefix-match each other: `handle_connection`
    /// dispatches on `starts_with`, so a shared prefix would route About to
    /// Settings and reintroduce exactly the silent-wrong-window bug this verb
    /// was added to fix.
    #[test]
    fn surface_verbs_are_distinguishable() {
        assert_ne!(SURFACE_VERB, SURFACE_ABOUT_VERB);
        assert!(!SURFACE_ABOUT_VERB.starts_with(SURFACE_VERB));
        assert!(!SURFACE_VERB.starts_with(SURFACE_ABOUT_VERB));
    }

    /// Every view reachable by `--open-view` needs a control verb, or a second
    /// launch silently opens the wrong one.
    #[test]
    fn every_view_has_a_surface_verb() {
        for view in observer_model::View::ALL {
            let verb: &[u8] = match view {
                observer_model::View::Settings => SURFACE_VERB,
                observer_model::View::About => SURFACE_ABOUT_VERB,
            };
            assert!(verb.ends_with(b"\n"), "{} verb must be newline-terminated", view.label());
        }
    }
}
