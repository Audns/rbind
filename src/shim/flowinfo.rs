//! IPv6 flowinfo forcing.
//!
//! Activated by `FLOWINFO=0xN` where the upper 12 bits are the
//! traffic class and the lower 20 bits are the flowlabel. We:
//!
//!  1. Apply the forced value to `sin6_flowinfo` of the *destination*
//!     sockaddr (done in [`crate::hooks::alter_dest_sa`] via
//!     [`override_dest_flowinfo`]).
//!  2. Once per socket, install an `IPV6_FLOWLABEL_MGR` so the kernel
//!     knows which flowlabel to use, then enable `IPV6_FLOWINFO_SEND`.
//!
//! Both steps are gated on `cfg.force_flowinfo.is_some()` and (for the
//! kernel install) on the socket being `AF_INET6`.

#![allow(dead_code)]

use libc::{AF_INET6, SOL_IPV6, c_int, c_void, socklen_t};

use crate::shim::consts::{
    IPV6_FL_A_GET, IPV6_FL_F_CREATE, IPV6_FL_S_ANY, IPV6_FLOWINFO_SEND, IPV6_FLOWLABEL_MASK,
    IPV6_FLOWLABEL_MGR, in6_flowlabel_req,
};
use crate::shim::socket::SocketInfo;
use crate::shim::syscalls::OLD_SETSOCKOPT;
use crate::xlog;

/// Override the `sin6_flowinfo` field of the destination `sockaddr_storage`
/// in place if a flowinfo value is forced. Logs the change at level 1.
///
/// No-op for non-IPv6 sockaddrs or when no flowinfo is forced.
pub fn override_dest_flowinfo(sockfd: c_int, ss: &mut libc::sockaddr_storage) {
    let cfg = crate::shim::init::init();
    let Some(forced) = cfg.force_flowinfo else {
        return;
    };
    if c_int::from(ss.ss_family) != AF_INET6 {
        return;
    }

    // Safety: family is AF_INET6, so reinterpreting as sockaddr_in6 is sound
    // (sockaddr_storage is at least sizeof(sockaddr_in6) bytes).
    unsafe {
        let s6 = crate::shim::sockaddr::as_in6_mut(ss);
        let old = u32::from_be(s6.sin6_flowinfo);
        xlog!(
            1,
            "changing flowinfo from 0x{:x} to 0x{:x} [{}]!\n",
            old,
            forced,
            sockfd
        );
        s6.sin6_flowinfo = forced.to_be();
    }
}

/// Issue the two `setsockopt` calls that install a flowlabel manager and
/// enable emission of flowinfo on send. Once per socket (gated by
/// [`SocketInfo::is_flowinfo_called`]).
///
/// Mark flowinfo-called even on error — the C version does the same, on
/// the rationale that retrying won't help if the kernel rejected the
/// manager install.
pub fn install_for_socket(sockfd: c_int, info: &mut SocketInfo) {
    let cfg = crate::shim::init::init();
    let Some(forced) = cfg.force_flowinfo else {
        return;
    };
    if info.domain != AF_INET6 {
        return;
    }
    if info.is_flowinfo_called() {
        return;
    }
    info.mark_flowinfo_called();

    // Build the flowlabel manager request from the *destination* address
    // we recorded in SocketInfo::dest.
    let mut mgr: in6_flowlabel_req = unsafe { std::mem::zeroed() };
    unsafe {
        let s6 = info
            .dest
            .as_ptr()
            .cast::<libc::sockaddr_in6>()
            .read_unaligned();
        mgr.flr_dst = s6.sin6_addr;
    }
    // flr_label is __be32; convert from host order (the configured value).
    mgr.flr_label = (forced & IPV6_FLOWLABEL_MASK).to_be();
    mgr.flr_action = IPV6_FL_A_GET;
    mgr.flr_share = IPV6_FL_S_ANY;
    mgr.flr_flags = IPV6_FL_F_CREATE;

    let fp = OLD_SETSOCKOPT
        .get()
        .expect("OLD_SETSOCKOPT not initialized");

    let ret = unsafe {
        fp(
            sockfd,
            SOL_IPV6,
            IPV6_FLOWLABEL_MGR,
            (&raw const mgr).cast::<c_void>(),
            std::mem::size_of::<in6_flowlabel_req>() as socklen_t,
        )
    };
    let e = std::io::Error::last_os_error();
    xlog!(1, "flow mgr (ret={}({})) [{}].\n", ret, e, sockfd);

    let yes: c_int = 1;
    let ret = unsafe {
        fp(
            sockfd,
            SOL_IPV6,
            IPV6_FLOWINFO_SEND,
            (&raw const yes).cast::<c_void>(),
            std::mem::size_of::<c_int>() as socklen_t,
        )
    };
    let e = std::io::Error::last_os_error();
    xlog!(
        1,
        "changing flowinfo to 'yes' (ret={}({})) [{}].\n",
        ret,
        e,
        sockfd
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    #[test]
    fn flowlabel_req_layout() {
        // The kernel ABI expects exactly this size; if we drift, the
        // setsockopt(IPV6_FLOWLABEL_MGR) call will silently corrupt
        // adjacent memory.
        assert_eq!(size_of::<in6_flowlabel_req>(), 32);
    }
}
