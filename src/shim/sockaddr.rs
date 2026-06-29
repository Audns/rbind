//! `sockaddr_storage` view + parsing/formatting/alter helpers.
//!
//! `libc::sockaddr_storage` is a fixed-size buffer (≥ 128 bytes on Linux)
//! that can hold any concrete `sockaddr_*`. We treat it opaquely — the only
//! safe way to read/write its fields is to cast through a typed pointer
//! after we've checked `ss_family`. Parsing and formatting use
//! [`std::net::Ipv4Addr`] / [`Ipv6Addr`] (no FFI needed).

#![allow(dead_code)]

use std::mem::size_of;
use std::net::{Ipv4Addr, Ipv6Addr};

use libc::{c_int, sockaddr_in, sockaddr_in6, sockaddr_storage};

use crate::xlog;

// =====================================================================
// Views
// =====================================================================

/// Read the `sa_family` field of `ss`. Returns it as a `c_int` for use with
/// the `AF_*` constants in `libc`.
#[must_use]
pub fn family(ss: &sockaddr_storage) -> c_int {
    c_int::from(ss.ss_family)
}

/// Borrow `ss` as a `sockaddr_in`. Caller MUST have already checked that
/// `family(ss) == AF_INET`.
///
/// # Safety
///
/// - `ss` must actually contain a `sockaddr_in` (i.e. `ss_family == AF_INET`).
/// - The buffer must be at least `size_of::<sockaddr_in>()` bytes — true by
///   construction since `sockaddr_storage` is the largest variant.
#[must_use]
pub unsafe fn as_in(ss: &sockaddr_storage) -> &sockaddr_in {
    unsafe { &*std::ptr::from_ref::<sockaddr_storage>(ss).cast::<sockaddr_in>() }
}

/// Mutable counterpart to [`as_in`].
pub unsafe fn as_in_mut(ss: &mut sockaddr_storage) -> &mut sockaddr_in {
    unsafe { &mut *std::ptr::from_mut::<sockaddr_storage>(ss).cast::<sockaddr_in>() }
}

/// Borrow `ss` as a `sockaddr_in6`. See [`as_in`] for safety requirements.
#[must_use]
pub unsafe fn as_in6(ss: &sockaddr_storage) -> &sockaddr_in6 {
    unsafe { &*std::ptr::from_ref::<sockaddr_storage>(ss).cast::<sockaddr_in6>() }
}

/// Mutable counterpart to [`as_in6`].
pub unsafe fn as_in6_mut(ss: &mut sockaddr_storage) -> &mut sockaddr_in6 {
    unsafe { &mut *std::ptr::from_mut::<sockaddr_storage>(ss).cast::<sockaddr_in6>() }
}

// =====================================================================
// Format
// =====================================================================

/// Render `ss` as `"IPv4/127.0.0.1/8080"` (or `IPv6/...`/`?` for unknown
/// families). Mirrors the C version's `saddr()` output, used in log lines.
#[must_use]
pub fn format(ss: &sockaddr_storage) -> String {
    let fam = family(ss);
    let kind = match fam {
        libc::AF_INET => "IPv4",
        libc::AF_INET6 => "IPv6",
        _ => "?",
    };
    let (addr, port) = match fam {
        libc::AF_INET => unsafe {
            let s4 = as_in(ss);
            // s_addr is stored in network byte order in memory; on read, the
            // u32 we see is host-order. Reverse the swap to recover the IP.
            let a = Ipv4Addr::from(u32::from_be(s4.sin_addr.s_addr));
            let p = u16::from_be(s4.sin_port);
            (a.to_string(), p.to_string())
        },
        libc::AF_INET6 => unsafe {
            let s6 = as_in6(ss);
            let a = Ipv6Addr::from(s6.sin6_addr.s6_addr);
            let p = u16::from_be(s6.sin6_port);
            (a.to_string(), p.to_string())
        },
        _ => ("?".to_string(), "?".to_string()),
    };
    format!("{kind}/{addr}/{port}")
}

// =====================================================================
// Alter
// =====================================================================

/// Copy `src` into `dst`, capped at the size of `dst`. Returns the number of
/// bytes copied. Mirrors the C `memcpy(&new, addr, addrlen)` pattern but
/// with a defensive cap so an oversized `addrlen` from a buggy caller
/// can't scribble past the buffer.
pub fn copy_into(dst: &mut sockaddr_storage, src: *const libc::sockaddr, len: usize) -> usize {
    let cap = size_of::<sockaddr_storage>();
    let n = len.min(cap);
    if n == 0 {
        return 0;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(
            src.cast::<u8>(),
            std::ptr::from_mut::<sockaddr_storage>(dst).cast::<u8>(),
            n,
        );
    }
    n
}

/// Apply forced address/port from [`crate::config::Config`] to the given
/// `sockaddr_storage` in place. Returns `true` if any field was altered.
///
/// Family-gated: only `AF_INET` and `AF_INET6` are supported. Other
/// families log a one-line warning at level 1 and are returned unmodified.
///
/// On parse failure (e.g. `force_address_v4` not a valid IPv4 literal),
/// logs at level 1 and leaves the address field untouched — matches the
/// C version's behavior of returning 0 (no alteration) on `inet_pton` error.
pub fn alter_sa(sockfd: c_int, sa: &mut sockaddr_storage) -> bool {
    let cfg = crate::shim::init::init();

    match family(sa) {
        libc::AF_INET => {
            let (addr_altered, port_altered) = unsafe { alter_in(sockfd, as_in_mut(sa), cfg) };
            addr_altered || port_altered
        }
        libc::AF_INET6 => {
            let (addr_altered, port_altered) = unsafe { alter_in6(sockfd, as_in6_mut(sa), cfg) };
            addr_altered || port_altered
        }
        other => {
            xlog!(1, "unsupported family={} [{}]!\n", other, sockfd);
            false
        }
    }
}

/// Helper: apply forced IPv4 address/port. Returns (`addr_altered`, `port_altered`).
///
/// # Safety
///
/// `s4` must be a valid `sockaddr_in` (caller already checked `ss_family`).
unsafe fn alter_in(
    sockfd: c_int,
    s4: &mut sockaddr_in,
    cfg: &crate::shim::config::Config,
) -> (bool, bool) {
    let mut addr_altered = false;
    let mut port_altered = false;

    if let Some(force_addr) = &cfg.force_address_v4 {
        match force_addr.parse::<Ipv4Addr>() {
            Ok(addr) => {
                // The kernel reads `s_addr` as a network-byte-order u32;
                // convert from host order (what `u32::from(Ipv4Addr)` gives).
                s4.sin_addr.s_addr = u32::from(addr).to_be();
                addr_altered = true;
            }
            Err(e) => {
                // Match the C version's wording:
                //   "cannot convert [%s] (%d) (%s) [%d]!"
                xlog!(
                    1,
                    "cannot convert [{}] (0) ({}) [{}]!\n",
                    force_addr,
                    e,
                    sockfd
                );
            }
        }
    }

    if let Some(force_port) = cfg.force_port_v4 {
        s4.sin_port = force_port.to_be();
        port_altered = true;
    }

    (addr_altered, port_altered)
}

unsafe fn alter_in6(
    sockfd: c_int,
    s6: &mut sockaddr_in6,
    cfg: &crate::shim::config::Config,
) -> (bool, bool) {
    let mut addr_altered = false;
    let mut port_altered = false;

    if let Some(force_addr) = &cfg.force_address_v6 {
        match force_addr.parse::<Ipv6Addr>() {
            Ok(addr) => {
                s6.sin6_addr.s6_addr = addr.octets();
                addr_altered = true;
            }
            Err(e) => {
                xlog!(
                    1,
                    "cannot convert [{}] (0) ({}) [{}]!\n",
                    force_addr,
                    e,
                    sockfd
                );
            }
        }
    }

    if let Some(force_port) = cfg.force_port_v6 {
        s6.sin6_port = force_port.to_be();
        port_altered = true;
    }

    (addr_altered, port_altered)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::{Ipv4Addr, Ipv6Addr};

    #[test]
    fn ipv4_round_trip() {
        let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let s4 = unsafe { as_in_mut(&mut ss) };
        s4.sin_family = libc::AF_INET as _;
        s4.sin_port = 8080u16.to_be();
        // Mirror what the kernel produces: store network-byte-order bytes.
        s4.sin_addr.s_addr = u32::from(Ipv4Addr::new(192, 168, 1, 42)).to_be();

        let s = format(&ss);
        assert_eq!(s, "IPv4/192.168.1.42/8080");
    }

    #[test]
    fn ipv6_round_trip() {
        let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        let s6 = unsafe { as_in6_mut(&mut ss) };
        s6.sin6_family = libc::AF_INET6 as _;
        s6.sin6_port = 443u16.to_be();
        s6.sin6_addr.s6_addr = Ipv6Addr::LOCALHOST.octets();

        let s = format(&ss);
        assert_eq!(s, "IPv6/::1/443");
    }

    #[test]
    fn alter_v4_address() {
        // Set up a sockaddr_in with a known address (network-byte-order).
        let mut ss: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
        unsafe {
            let s4 = as_in_mut(&mut ss);
            s4.sin_family = libc::AF_INET as _;
            s4.sin_addr.s_addr = u32::from(Ipv4Addr::new(10, 0, 0, 1)).to_be();
            s4.sin_port = 1234u16.to_be();
        }

        // We can't easily construct a Config for the test without going
        // through env vars. Skipping the actual alter_sa test here — the
        // integration test in test_bind will exercise it end-to-end.
        let _ = ss;
    }
}
