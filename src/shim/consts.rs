//! Local shims for libc constants and types that the `libc` crate does not
//! guarantee on every Linux target/version.
//!
//! Everything here is `pub use`d from [`crate`] so callers always go through
//! a single point of truth rather than sprinkling `libc::` imports.

#![allow(non_camel_case_types, dead_code)]

// `compile_error!` cfg gate below flips on Linux/Android only — keep `use`s
// unconditional so we don't need an outer cfg attribute that rust-analyzer
// reads as "inactive" code.
#[allow(unused_imports)]
use libc::{c_int, c_uchar, c_uint, c_ushort};

// Build target gate. This shim is Linux-only; the rewrite doesn't aim to
// support other platforms.
#[cfg(not(any(target_os = "linux", target_os = "android")))]
compile_error!("rbind only supports Linux/Android targets");

// === Socket options / socket types that may be missing from `libc` ===

/// `SO_MARK` from `<linux/socket.h>`. Value 36 is consistent across all Linux
/// architectures; some `libc` versions don't re-export it.
pub const SO_MARK: c_int = 36;

/// `SOCK_DCCP` (Datagram Congestion Control Protocol). Added to glibc
/// relatively late; not all `libc` versions expose it.
pub const SOCK_DCCP: c_int = 6;

// === IPv6 flowinfo ===

/// `IPV6_FLOWLABEL_MGR` — install a flowlabel manager for a socket.
pub const IPV6_FLOWLABEL_MGR: c_int = 32;
/// `IPV6_FLOWINFO_SEND` — tell the kernel to emit flowinfo on send.
pub const IPV6_FLOWINFO_SEND: c_int = 33;

/// `IPV6_FL_F_CREATE` — create the flowlabel if it doesn't exist.
/// Stored in `flr_flags` which is `__u16` in the kernel struct.
pub const IPV6_FL_F_CREATE: c_ushort = 1;
/// `IPV6_FL_A_GET` — acquire (read-only) the flowlabel.
/// Stored in `flr_action` which is `__u8`.
pub const IPV6_FL_A_GET: c_uchar = 0;
/// `IPV6_FL_S_ANY` — any sharing mode. Stored in `flr_share` (`__u8`);
/// value 255 fits in `u8`. (The kernel also defines `IPV6_FL_S_USER = 256`
/// which would need u16; we don't use it.)
pub const IPV6_FL_S_ANY: c_uchar = 255;

/// Mask for the entire `sin6_flowinfo` field (class + label).
pub const IPV6_FLOWINFO_MASK: c_uint = 0x0FFF_FFFF;
/// Mask for just the flowlabel portion.
pub const IPV6_FLOWLABEL_MASK: c_uint = 0x000F_FFFF;

// === `struct in6_flowlabel_req` ===

/// `struct in6_flowlabel_req` from `<linux/ipv6.h>`. Not exposed by `libc` on
/// every target, so we ship our own `#[repr(C)]` shim. Layout MUST match
/// the kernel header — `setsockopt(IPV6_FLOWLABEL_MGR)` is meaningless if it
/// doesn't.
#[repr(C)]
#[derive(Copy, Clone)]
pub struct in6_flowlabel_req {
    pub flr_dst: libc::in6_addr,
    pub flr_label: c_uint,
    pub flr_action: c_uchar,
    pub flr_share: c_uchar,
    pub flr_flags: c_ushort,
    pub flr_expires: c_ushort,
    pub flr_linger: c_ushort,
    // pub __flr_pad: c_uint,
}
