//! Forced-setsockopt intercept table.
//!
//! Mirrors the C version's `setsockopt()` body: when the caller asks for one
//! of a known set of (level, optname) pairs, we *replace* the caller's value
//! with whatever the corresponding short env var (`TOS`, `TTL`, `BIND_DEVICE`,
//! …) dictates. All other
//! pairs fall through to the real libc `setsockopt`.
//!
//! Each individual [`set_*`] function is also called from the socket-create
//! callback (steps 5+) so every freshly-opened socket gets the forced
//! options applied up front, regardless of whether the host program ever
//! calls `setsockopt` on its own.
//!
//! ## Why match the C version's log format?
//!
//! The C version's log lines are a regression baseline — the test scripts
//! and human operators read them. Each `set_*` function logs at level 1
//! using the same wording and field order as the C code.

#![allow(dead_code)]

use libc::{c_int, c_void, socklen_t};

use crate::shim::consts::SO_MARK;
use crate::shim::syscalls::OLD_SETSOCKOPT;
use crate::xlog;

/// Decide whether `(level, optname)` is one we intercept.
///
/// Returns `Some(ret)` when the pair is one of ours — in that case the caller
/// must return `ret` *without* forwarding the call to libc. Returns `None` to
/// signal "fall through to the real `setsockopt(2)`".
#[must_use]
pub fn intercept(sockfd: c_int, level: c_int, optname: c_int) -> Option<c_int> {
    use libc::{IP_TOS, IP_TTL};
    use libc::{IPPROTO_IP, IPPROTO_TCP, SOL_SOCKET};
    use libc::{SO_BINDTODEVICE, SO_KEEPALIVE, SO_PRIORITY, SO_REUSEADDR};
    use libc::{TCP_KEEPIDLE, TCP_MAXSEG, TCP_NODELAY};

    match (level, optname) {
        (SOL_SOCKET, SO_KEEPALIVE) => Some(set_ka(sockfd)),
        (SOL_SOCKET, SO_REUSEADDR) => Some(set_reuseaddr(sockfd)),
        (SOL_SOCKET, SO_MARK) => Some(set_fwmark(sockfd)),
        (SOL_SOCKET, SO_PRIORITY) => Some(set_prio(sockfd)),
        (SOL_SOCKET, SO_BINDTODEVICE) => Some(set_bind_to_device_from_env(sockfd)),
        (IPPROTO_IP, IP_TOS) => Some(set_tos(sockfd)),
        (IPPROTO_IP, IP_TTL) => Some(set_ttl(sockfd)),
        (IPPROTO_TCP, TCP_KEEPIDLE) => Some(set_ka_idle(sockfd)),
        (IPPROTO_TCP, TCP_MAXSEG) => Some(set_mss(sockfd)),
        (IPPROTO_TCP, TCP_NODELAY) => Some(set_nodelay(sockfd)),
        _ => None,
    }
}

// === Internals ===

/// Forward one `setsockopt(fd, level, optname, &value, sizeof(int))` call
/// through the resolved `OLD_SETSOCKOPT` pointer.
///
/// # Safety
///
/// Caller must guarantee `OLD_SETSOCKOPT` has been initialized (i.e. `init()`
/// has run). All public entry points in this module are guarded by
/// `crate::init::init()` first.
unsafe fn call_int(sockfd: c_int, level: c_int, optname: c_int, value: c_int) -> c_int {
    unsafe {
        let fp = OLD_SETSOCKOPT
            .get()
            .expect("OLD_SETSOCKOPT not initialized");
        fp(
            sockfd,
            level,
            optname,
            (&raw const value).cast::<c_void>(),
            std::mem::size_of::<c_int>() as socklen_t,
        )
    }
}

/// Render errno as a string for the log line. Called immediately after
/// `setsockopt` returns, before any other syscall can clobber `errno`.
fn errno_str() -> String {
    std::io::Error::last_os_error().to_string()
}

// === Individual forced setters ===

/// `SO_KEEPALIVE` — enable/disable keepalive based on the forced value.
/// `flag = (keepalive > 0) ? 1 : 0`, matching the C version.
#[must_use]
pub fn set_ka(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(keepalive) = cfg.force_keepalive else {
        return 0;
    };
    let flag: c_int = i32::from(keepalive > 0);
    let ret = unsafe { call_int(sockfd, libc::SOL_SOCKET, libc::SO_KEEPALIVE, flag) };
    let e = errno_str();
    xlog!(
        1,
        "changing SO_KEEPALIVE to {} (ret={}({})) [{}].\n",
        flag,
        ret,
        e,
        sockfd
    );
    ret
}

/// `TCP_KEEPIDLE` — keepalive idle time. Shares the `force_keepalive` config
/// with [`set_ka`] (the C version does the same).
#[must_use]
pub fn set_ka_idle(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(keepalive) = cfg.force_keepalive else {
        return 0;
    };
    let ret = unsafe {
        call_int(
            sockfd,
            libc::IPPROTO_TCP,
            libc::TCP_KEEPIDLE,
            keepalive.cast_signed(),
        )
    };
    let e = errno_str();
    xlog!(
        1,
        "changing TCP_KEEPIDLE to {}s (ret={}({})) [{}].\n",
        keepalive,
        ret,
        e,
        sockfd
    );
    ret
}

/// `TCP_MAXSEG`.
#[must_use]
pub fn set_mss(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(mss) = cfg.force_mss else { return 0 };
    let ret = unsafe {
        call_int(
            sockfd,
            libc::IPPROTO_TCP,
            libc::TCP_MAXSEG,
            mss.cast_signed(),
        )
    };
    let e = errno_str();
    xlog!(
        1,
        "changing MSS to {} (ret={}({})) [{}].\n",
        mss,
        ret,
        e,
        sockfd
    );
    ret
}

/// `IP_TOS`.
#[must_use]
pub fn set_tos(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(tos) = cfg.force_tos else { return 0 };
    let ret = unsafe { call_int(sockfd, libc::IPPROTO_IP, libc::IP_TOS, c_int::from(tos)) };
    let e = errno_str();
    xlog!(
        1,
        "changing TOS to {} (ret={}({})) [{}].\n",
        tos,
        ret,
        e,
        sockfd
    );
    ret
}

/// `IP_TTL`.
#[must_use]
pub fn set_ttl(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(ttl) = cfg.force_ttl else { return 0 };
    let ret = unsafe { call_int(sockfd, libc::IPPROTO_IP, libc::IP_TTL, c_int::from(ttl)) };
    let e = errno_str();
    xlog!(
        1,
        "changing TTL to {} (ret={}({})) [{}].\n",
        ttl,
        ret,
        e,
        sockfd
    );
    ret
}

/// `SO_REUSEADDR`.
#[must_use]
pub fn set_reuseaddr(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(reuseaddr) = cfg.force_reuseaddr else {
        return 0;
    };
    let ret = unsafe {
        call_int(
            sockfd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            reuseaddr.cast_signed(),
        )
    };
    let e = errno_str();
    xlog!(
        1,
        "changing reuseaddr to {} (ret={}({})) [{}].\n",
        reuseaddr,
        ret,
        e,
        sockfd
    );
    ret
}

/// `TCP_NODELAY`.
#[must_use]
pub fn set_nodelay(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(nodelay) = cfg.force_nodelay else {
        return 0;
    };
    let ret = unsafe {
        call_int(
            sockfd,
            libc::IPPROTO_TCP,
            libc::TCP_NODELAY,
            nodelay.cast_signed(),
        )
    };
    let e = errno_str();
    xlog!(
        1,
        "changing nodelay to {} (ret={}({})) [{}].\n",
        nodelay,
        ret,
        e,
        sockfd
    );
    ret
}

/// `SO_MARK`.
#[must_use]
pub fn set_fwmark(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(fwmark) = cfg.force_fwmark else {
        return 0;
    };
    let ret = unsafe { call_int(sockfd, libc::SOL_SOCKET, SO_MARK, fwmark.cast_signed()) };
    let e = errno_str();
    xlog!(
        1,
        "changing fwmark to 0x{:x} (ret={}({})) [{}].\n",
        fwmark,
        ret,
        e,
        sockfd
    );
    ret
}

/// `SO_PRIORITY`.
#[must_use]
pub fn set_prio(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(prio) = cfg.force_prio else { return 0 };
    let ret = unsafe {
        call_int(
            sockfd,
            libc::SOL_SOCKET,
            libc::SO_PRIORITY,
            prio.cast_signed(),
        )
    };
    let e = errno_str();
    xlog!(
        1,
        "changing prio to {} (ret={}({})) [{}].\n",
        prio,
        ret,
        e,
        sockfd
    );
    ret
}

/// `SO_BINDTODEVICE` — apply the env-var-forced interface name to `fd`.
///
/// This is the *socket-creation* apply path: called from
/// `socket_create_callback` so every freshly-opened socket is bound to the
/// forced interface. The intercept path (see [`intercept`]) calls
/// [`set_bind_to_device_from_env`] when the host program tries to set its own.
#[must_use]
pub fn set_bind_to_device(sockfd: c_int, ifname: &str) -> c_int {
    let ret = unsafe { set_bind_to_device_raw(sockfd, ifname) };
    let e = errno_str();
    xlog!(
        1,
        "changing BINDTODEVICE to {} (ret={}({})) [{}].\n",
        ifname,
        ret,
        e,
        sockfd
    );
    ret
}

/// Driven by the intercept table — same as [`set_bind_to_device`], but
/// called when the host program tries `setsockopt(SO_BINDTODEVICE, …)`
/// itself (so it gets overridden by the forced value).
fn set_bind_to_device_from_env(sockfd: c_int) -> c_int {
    let cfg = crate::shim::init::init();
    let Some(ifname) = cfg.force_bind_to_device.as_deref() else {
        return 0;
    };
    set_bind_to_device(sockfd, ifname)
}

/// Underlying raw call: encode `ifname` into a fixed `IFNAMSIZ` buffer and
/// forward to the resolved `setsockopt(2)`.
///
/// # Safety
///
/// Caller must guarantee `OLD_SETSOCKOPT` is initialized.
unsafe fn set_bind_to_device_raw(sockfd: c_int, ifname: &str) -> c_int {
    unsafe {
        let fp = OLD_SETSOCKOPT
            .get()
            .expect("OLD_SETSOCKOPT not initialized");

        // SO_BINDTODEVICE wants a NUL-terminated string up to IFNAMSIZ bytes.
        // We don't need the NUL (the kernel uses the `optlen` we pass) but
        // truncating to IFNAMSIZ is required to avoid EFAULT.
        let mut buf = [0u8; libc::IFNAMSIZ];
        let bytes = ifname.as_bytes();
        let n = bytes.len().min(libc::IFNAMSIZ);
        buf[..n].copy_from_slice(&bytes[..n]);

        fp(
            sockfd,
            libc::SOL_SOCKET,
            libc::SO_BINDTODEVICE,
            buf.as_ptr().cast::<c_void>(),
            n as socklen_t,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intercept_returns_none_for_unknown_pair() {
        // SOL_SOCKET + SO_KEEPALIVE is known, but SOL_SOCKET + SO_SNDBUF is not.
        assert!(intercept(-1, libc::SOL_SOCKET, libc::SO_SNDBUF).is_none());
        assert!(intercept(-1, 0, 0).is_none());
    }
}
