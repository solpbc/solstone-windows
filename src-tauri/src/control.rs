// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

use std::io::Write;
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

use tokio::io::AsyncReadExt;

pub const CONTROL_PORT: u16 = 49248;

const SURFACE_VERB: &[u8] = b"surface-settings\n";

pub fn signal_surface() -> bool {
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

    stream.write_all(SURFACE_VERB).is_ok()
}

pub async fn serve(app: tauri::AppHandle, listener: tokio::net::TcpListener) {
    loop {
        let (stream, _) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => continue,
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
    if buf[..n].starts_with(SURFACE_VERB) {
        std::thread::spawn(move || {
            let _ = crate::windows::open_settings(&app);
        });
    }
}
