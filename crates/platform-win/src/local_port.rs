// SPDX-License-Identifier: AGPL-3.0-only
// Copyright (c) 2026 sol pbc

//! Whether whatever listens on, or connects to, a loopback TCP port runs under
//! this account.
//!
//! A refusal, a process that can't be opened, or no matching row at all reads
//! as false. Another account's process can't be opened from here, so it reads
//! as false too. The row's connection state is never read: it says nothing
//! about whose connection this is.

#[cfg(windows)]
pub use imp::{client_is_this_account, listener_is_this_account};

/// Off Windows there is no owner to read, so nothing is ever claimed as ours.
#[cfg(not(windows))]
pub fn listener_is_this_account(_port: u16) -> bool {
    false
}

/// Off Windows there is no account to read, so a connection isn't ours either.
#[cfg(not(windows))]
pub fn client_is_this_account(
    _server: std::net::SocketAddrV4,
    _client: std::net::SocketAddrV4,
) -> bool {
    false
}

#[cfg(windows)]
mod imp {
    use std::ffi::c_void;

    use windows::Win32::Foundation::{ERROR_INSUFFICIENT_BUFFER, NO_ERROR};
    use windows::Win32::NetworkManagement::IpHelper::{
        GetExtendedTcpTable, MIB_TCPROW_OWNER_PID, MIB_TCPTABLE_OWNER_PID, TCP_TABLE_CLASS,
        TCP_TABLE_OWNER_PID_ALL, TCP_TABLE_OWNER_PID_LISTENER,
    };
    use windows::Win32::Networking::WinSock::AF_INET;

    /// Every listener a connection to `127.0.0.1:port` could reach must be ours.
    pub fn listener_is_this_account(port: u16) -> bool {
        let pids = loopback_listener_pids(port);
        !pids.is_empty()
            && pids
                .iter()
                .all(|pid| crate::browser_pipe::process_is_current_user(*pid).unwrap_or(false))
    }

    /// The process at the far end of an accepted connection must be ours.
    /// Blocking, and about 10 ms with thousands of connections open.
    pub fn client_is_this_account(
        server: std::net::SocketAddrV4,
        client: std::net::SocketAddrV4,
    ) -> bool {
        let address = |socket: std::net::SocketAddrV4| u32::from_ne_bytes(socket.ip().octets());
        let pids: Vec<u32> = tcp_rows(TCP_TABLE_OWNER_PID_ALL)
            .into_iter()
            // The client's row reads from its side: its address is local, ours is remote.
            .filter(|row| {
                row.dwOwningPid != 0
                    && row.dwLocalAddr == address(client)
                    && u16::from_be(row.dwLocalPort as u16) == client.port()
                    && row.dwRemoteAddr == address(server)
                    && u16::from_be(row.dwRemotePort as u16) == server.port()
            })
            .map(|row| row.dwOwningPid)
            .collect();
        !pids.is_empty()
            && pids
                .iter()
                .all(|pid| crate::browser_pipe::process_is_current_user(*pid).unwrap_or(false))
    }

    fn loopback_listener_pids(port: u16) -> Vec<u32> {
        const LOOPBACK: u32 = u32::from_ne_bytes([127, 0, 0, 1]);
        const ANY: u32 = 0;
        tcp_rows(TCP_TABLE_OWNER_PID_LISTENER)
            .into_iter()
            .filter(|row| {
                u16::from_be(row.dwLocalPort as u16) == port
                    && matches!(row.dwLocalAddr, LOOPBACK | ANY)
            })
            .map(|row| row.dwOwningPid)
            .collect()
    }

    fn tcp_rows(class: TCP_TABLE_CLASS) -> Vec<MIB_TCPROW_OWNER_PID> {
        let mut size = 0u32;
        // The table can grow between calls, so size and read until it fits.
        for _ in 0..4 {
            // u32 words keep the table's alignment.
            let mut buffer = vec![0u32; (size as usize).div_ceil(4)];
            let pointer = (!buffer.is_empty()).then_some(buffer.as_mut_ptr() as *mut c_void);
            let result = unsafe {
                GetExtendedTcpTable(pointer, &mut size, false, AF_INET.0 as u32, class, 0)
            };
            if result == NO_ERROR.0 {
                if pointer.is_none() {
                    return Vec::new();
                }
                let table = buffer.as_ptr() as *const MIB_TCPTABLE_OWNER_PID;
                let rows = unsafe {
                    std::slice::from_raw_parts(
                        std::ptr::addr_of!((*table).table) as *const MIB_TCPROW_OWNER_PID,
                        (*table).dwNumEntries as usize,
                    )
                };
                return rows.to_vec();
            }
            if result != ERROR_INSUFFICIENT_BUFFER.0 {
                return Vec::new();
            }
        }
        Vec::new()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use std::net::TcpListener;

        #[test]
        fn a_client_in_this_process_is_ours_and_a_stranger_is_not() {
            use std::net::{SocketAddr, SocketAddrV4, TcpStream};
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let SocketAddr::V4(server) = listener.local_addr().unwrap() else {
                unreachable!()
            };
            let _client = TcpStream::connect(server).unwrap();
            let (_accepted, peer) = listener.accept().unwrap();
            let SocketAddr::V4(peer) = peer else {
                unreachable!()
            };
            assert!(client_is_this_account(server, peer));
            let stranger = SocketAddrV4::new(*peer.ip(), peer.port().wrapping_add(1));
            assert!(!client_is_this_account(server, stranger));
        }

        #[test]
        fn a_listener_this_process_opened_is_ours() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            assert!(listener_is_this_account(
                listener.local_addr().unwrap().port()
            ));
        }

        #[test]
        fn a_port_nothing_listens_on_is_not_ours() {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let port = listener.local_addr().unwrap().port();
            drop(listener);
            assert!(!listener_is_this_account(port));
        }

        #[test]
        fn a_system_process_listener_is_not_ours() {
            // Port 135 is the RPC endpoint mapper on every Windows, owned by svchost
            // under a service account.
            assert!(!listener_is_this_account(135));
        }
    }
}
