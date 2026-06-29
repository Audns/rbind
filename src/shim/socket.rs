//! Per-fd socket metadata.
//!
//! Replaces the C version's `struct private`. Each open socket we track has
//! one [`SocketInfo`] entry in the global fd table.
//!
//! ## Fields
//!
//! - `domain`, `type_` — captured at `socket(2)` / `accept(2)` time so we can
//!   re-derive behavior (e.g. set TCP-only options) without a second syscall.
//! - `flags` — bitmask of [`FB_FLAGS_NETSOCK`] / [`FB_FLAGS_BIND_CALLED`] /
//!   [`FB_FLAGS_FLOWINFO_CALLED`], matching the C `FB_FLAGS_*` defines.
//! - `dest` — last destination address observed (e.g. via `connect`/`sendto`).
//!   Stored as a raw byte buffer so we can copy-in/copy-out any sockaddr family
//!   without unsafe view casts at every read.
//! - `dest_len` — populated length of `dest`.
//! - `limit`, `rest`, `last` — bandwidth throttling state (used by `bw.rs`).
//!
//! ## Memory layout of `dest`
//!
//! `dest` is exactly `size_of::<libc::sockaddr_storage>()` bytes (≥ 128 on
//! Linux). To read or write the underlying address family / port / address,
//! use the helper functions that will live in a future `sockaddr.rs` module.

#![allow(dead_code)]

use std::mem::size_of;
use std::time::Instant;

use libc::{c_int, socklen_t};

/// Size of the raw `sockaddr_storage` byte buffer used by [`SocketInfo::dest`].
pub const SOCKADDR_STORAGE_SIZE: usize = size_of::<libc::sockaddr_storage>();

/// Raw byte buffer big enough to hold any `struct sockaddr_storage`.
pub type SockAddrStorageBuf = [u8; SOCKADDR_STORAGE_SIZE];

// Flag bits — must match the C `FB_FLAGS_*` defines.
pub const FB_FLAGS_NETSOCK: u32 = 1 << 0;
pub const FB_FLAGS_BIND_CALLED: u32 = 1 << 1;
pub const FB_FLAGS_FLOWINFO_CALLED: u32 = 1 << 2;

#[derive(Clone)]
pub struct SocketInfo {
    pub domain: c_int,
    pub type_: c_int,
    pub flags: u32,
    pub dest: SockAddrStorageBuf,
    pub dest_len: socklen_t,
    pub limit: u64,
    pub rest: u64,
    pub last: Option<Instant>,
}

impl SocketInfo {
    /// Construct a fresh entry for a freshly-created socket. The
    /// `FB_FLAGS_NETSOCK` flag is set; everything else is zeroed.
    #[must_use]
    pub fn new(domain: c_int, type_: c_int) -> Self {
        Self {
            domain,
            type_,
            flags: FB_FLAGS_NETSOCK,
            dest: [0u8; SOCKADDR_STORAGE_SIZE],
            dest_len: 0,
            limit: 0,
            rest: 0,
            last: None,
        }
    }

    #[must_use]
    pub fn is_netsock(&self) -> bool {
        self.flags & FB_FLAGS_NETSOCK != 0
    }

    #[must_use]
    pub fn is_bind_called(&self) -> bool {
        self.flags & FB_FLAGS_BIND_CALLED != 0
    }

    #[must_use]
    pub fn is_flowinfo_called(&self) -> bool {
        self.flags & FB_FLAGS_FLOWINFO_CALLED != 0
    }

    pub fn mark_bind_called(&mut self) {
        self.flags |= FB_FLAGS_BIND_CALLED;
    }

    pub fn mark_flowinfo_called(&mut self) {
        self.flags |= FB_FLAGS_FLOWINFO_CALLED;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_sets_netsock_flag() {
        let s = SocketInfo::new(libc::AF_INET, libc::SOCK_STREAM);
        assert!(s.is_netsock());
        assert!(!s.is_bind_called());
        assert!(!s.is_flowinfo_called());
        assert_eq!(s.dest_len, 0);
        assert_eq!(s.limit, 0);
        assert_eq!(s.rest, 0);
        assert!(s.last.is_none());
    }

    #[test]
    fn mark_methods_set_flags() {
        let mut s = SocketInfo::new(libc::AF_INET, libc::SOCK_DGRAM);
        s.mark_bind_called();
        assert!(s.is_bind_called());
        assert!(s.is_netsock(), "NETSOCK flag must survive");
        s.mark_flowinfo_called();
        assert!(s.is_flowinfo_called());
    }

    #[test]
    fn dest_is_zeroed_on_new() {
        let s = SocketInfo::new(libc::AF_INET6, libc::SOCK_STREAM);
        assert!(s.dest.iter().all(|&b| b == 0));
    }
}
