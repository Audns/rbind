//! Hijacked libc symbols.
//!
//! Each `#[no_mangle] pub unsafe extern "C"` function in this module shadows
//! the corresponding glibc entry point when the resulting `.so` is loaded
//! via `LD_PRELOAD`. They share a common shape:
//!
//! 1. Call [`crate::init::init`] (idempotent — first call resolves libc
//!    symbols and loads config).
//! 2. Optional pre-processing (logging, rewriting args).
//! 3. Call through to the original `OLD_*` function pointer.
//! 4. Optional post-processing (apply forced setsockopt, register/unregister
//!    fd).
//!
//! Steps 5-10 add hook exports incrementally; this file will grow to host
//! all 11 hooks by step 10.

#![allow(non_camel_case_types, dead_code)]

use std::cell::Cell;

use libc::{c_int, socklen_t};

use crate::shim::bw;
use crate::shim::fd_table;
use crate::shim::setsockopt;
use crate::shim::sockaddr;
use crate::shim::socket::{SOCKADDR_STORAGE_SIZE, SocketInfo};
use crate::shim::syscalls::{
    OLD_ACCEPT, OLD_BIND, OLD_CLOSE, OLD_CONNECT, OLD_POLL, OLD_SEND, OLD_SENDMSG, OLD_SENDTO,
    OLD_SETSOCKOPT, OLD_SOCKET, OLD_WRITE,
};
use crate::xlog;

// =====================================================================
// Reentrance guard
//
// Our `xlog!` macro writes via `libc::write` to the log file fd. That call
// goes through the dynamic linker, hits our `write` hook, which would call
// `bw::throttle`, which would call `xlog!`, which would call `libc::write`,
// which would hit our `write` hook — infinite recursion (and eventual
// stack overflow / segfault).
//
// The fix is a thread-local "am I already inside a hook?" flag. When set,
// the hook body skips the side effects (logging, bandwidth throttling, fd
// table updates) and calls through to the original libc function. The
// `set`/`unset` is wrapped in a guard type so it resets on early return.
// =====================================================================

thread_local! {
    static IN_HOOK: Cell<bool> = const { Cell::new(false) };
}

/// If we're already inside a hook on this thread, return true. The caller
/// should call through to the original libc function and skip side effects.
fn is_reentrant() -> bool {
    IN_HOOK.with(std::cell::Cell::get)
}

/// Mark the current thread as "inside a hook". Returns a guard that clears
/// the flag on drop. If the flag was already set (reentrance), returns
/// `None` — the caller should pass through to the original libc function.
fn enter_hook() -> Option<HookGuard> {
    if IN_HOOK.with(std::cell::Cell::get) {
        None
    } else {
        IN_HOOK.with(|c| c.set(true));
        Some(HookGuard { active: true })
    }
}

/// RAII guard that resets `IN_HOOK` to false on drop. Constructed only via
/// [`enter_hook`]; if reentrance was detected, `enter_hook` returns `None`
/// and this guard is never created.
struct HookGuard {
    active: bool,
}

impl Drop for HookGuard {
    fn drop(&mut self) {
        if self.active {
            IN_HOOK.with(|c| c.set(false));
        }
    }
}

// =====================================================================
// Formatters used by the hooks' xlog lines
//
// The C version returns `char *` to a static buffer that gets clobbered on
// every call (a latent bug — calling sdomain() twice in one printf is UB).
// In Rust we just return owned Strings; the log macro accepts them by value.
// =====================================================================

fn sdomain(domain: c_int) -> String {
    match domain {
        libc::AF_INET => "IPv4".to_string(),
        libc::AF_INET6 => "IPv6".to_string(),
        n => n.to_string(),
    }
}

fn stype(t: c_int) -> String {
    // Mask off the type flags (SOCK_CLOEXEC = 0x80000, SOCK_NONBLOCK = 0x800)
    // so we classify the underlying socket type, matching the C version.
    match t & 0xfff {
        libc::SOCK_STREAM => "stream".to_string(),
        libc::SOCK_DGRAM => "dgram".to_string(),
        libc::SOCK_RAW => "raw".to_string(),
        libc::SOCK_SEQPACKET => "seqpacket".to_string(),
        crate::shim::consts::SOCK_DCCP => "dccp".to_string(),
        n => n.to_string(),
    }
}

fn sprotocol(protocol: c_int) -> String {
    match protocol {
        libc::IPPROTO_TCP => "tcp".to_string(),
        n => n.to_string(),
    }
}

// =====================================================================
// Shared socket-create callback
// =====================================================================

/// Apply every forced socket option to a freshly created fd, then register
/// the fd in the global fd table.
///
/// Called from both [`socket`] (step 5) and [`accept`] (step 10).
pub fn socket_create_callback(sockfd: c_int, domain: c_int, type_: c_int) {
    // Setsockopt return values are deliberately discarded: the C version
    // also ignores them, and stopping on the first forced-option failure
    // would surprise users who rely on partial-overrides (e.g. setting
    // FWMARK when SO_PRIORITY was already rejected by the kernel).
    let _ = setsockopt::set_tos(sockfd);
    let _ = setsockopt::set_ttl(sockfd);
    let _ = setsockopt::set_ka(sockfd);
    // TCP_KEEPIDLE only applies to stream sockets — matches the C version.
    if type_ & 0xfff == libc::SOCK_STREAM {
        let _ = setsockopt::set_ka_idle(sockfd);
    }
    let _ = setsockopt::set_mss(sockfd);
    let _ = setsockopt::set_reuseaddr(sockfd);
    let _ = setsockopt::set_nodelay(sockfd);
    let _ = setsockopt::set_fwmark(sockfd);
    let _ = setsockopt::set_prio(sockfd);
    if let Some(ifname) = crate::shim::init::init().force_bind_to_device.as_deref() {
        // Requires CAP_NET_RAW or root; we silently ignore the result so
        // an unprivileged host doesn't crash.
        let _ = setsockopt::set_bind_to_device(sockfd, ifname);
    }

    fd_table::add(sockfd, SocketInfo::new(domain, type_));
}

// =====================================================================
// Hook: socket(2)
// =====================================================================

/// `int socket(int domain, int type, int protocol);`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn socket(domain: c_int, type_: c_int, protocol: c_int) -> c_int {
    unsafe {
        crate::shim::init::init();

        // Set IN_HOOK *before* any xlog! / fd_table ops so that any re-entrant
        // hook (e.g. `write` via xlog → libc::write) sees the flag and skips
        // its side effects. Otherwise bw::throttle inside the re-entrant write
        // hook would lock the fd_table Mutex from inside libc::write — which
        // deadlocks against glibc's internal allocator locks.
        let _guard = enter_hook();

        xlog!(
            1,
            "socket(domain={}, type={}, protocol={})\n",
            sdomain(domain),
            stype(type_),
            sprotocol(protocol)
        );

        let fp = OLD_SOCKET.get().expect("OLD_SOCKET not initialized");
        let sockfd = fp(domain, type_, protocol);
        if sockfd == -1 {
            return -1;
        }

        socket_create_callback(sockfd, domain, type_);
        sockfd
    }
}

// =====================================================================
// Hook: close(2)
// =====================================================================

/// `int close(int fd);`
///
/// We deliberately do NOT special-case `stderr`/`stdout`/`stdin` fds — the
/// C version removes every fd from the table, even non-sockets. If the host
/// program closes fd 0/1/2, that's a `del` for a non-existent key, which is
/// a no-op (`HashMap::remove` returns `false`).
#[unsafe(no_mangle)]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    unsafe {
        crate::shim::init::init();
        xlog!(1, "close(fd={})\n", fd);

        // Whether or not the fd was tracked, we always close. Match the C version.
        let _ = fd_table::del(fd);

        let fp = OLD_CLOSE.get().expect("OLD_CLOSE not initialized");
        fp(fd)
    }
}

// =====================================================================
// Hook: bind(2)
// =====================================================================

/// `int bind(int sockfd, const struct sockaddr *addr, socklen_t addrlen);`
///
/// Behavior:
/// - For tracked network sockets (`AF_INET` / `AF_INET6`), the `addr` may be
///   rewritten by [`sockaddr::alter_sa`] using `ADDRESS_*` / `PORT_*`.
/// - If `ADDRESS_V4=deny` (or v6), the bind fails with `EACCES`.
/// - If `ADDRESS_V4=fake` (or v6), we return success without
///   calling the kernel — the socket will never accept connections.
/// - For untracked fds or non-network sockets, we pass through unmodified.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn bind(
    sockfd: c_int,
    addr: *const libc::sockaddr,
    addrlen: libc::socklen_t,
) -> c_int {
    unsafe {
        crate::shim::init::init();

        // Copy the caller's sockaddr into a local buffer for both logging
        // (via sockaddr::format) and modification.
        let mut tmp: libc::sockaddr_storage = std::mem::zeroed();
        sockaddr::copy_into(&mut tmp, addr, addrlen as usize);
        let tmp_str = sockaddr::format(&tmp);
        xlog!(1, "bind(sockfd={}, {})\n", sockfd, tmp_str);

        let mut new: libc::sockaddr_storage = std::mem::zeroed();
        sockaddr::copy_into(&mut new, addr, addrlen as usize);

        // Look up the fd. If it's not in our table, or it is but not flagged
        // as a network socket, fall through unmodified.
        let info = fd_table::get(sockfd);
        if let Some(info) = info
            && info.is_netsock()
        {
            let cfg = crate::shim::init::init();
            let force_address = match info.domain {
                libc::AF_INET => cfg.force_address_v4.as_deref(),
                libc::AF_INET6 => cfg.force_address_v6.as_deref(),
                _ => None,
            };

            // deny mode
            if let Some(a) = force_address {
                if a == "deny" {
                    xlog!(1, "\tDeny binding to {}\n", tmp_str);
                    set_errno(libc::EACCES);
                    return -1;
                }
                if a == "fake" {
                    xlog!(1, "\tFake binding to {}\n", tmp_str);
                    return 0;
                }
            }

            // Normal mode: rewrite address/port, mark bind-called.
            sockaddr::alter_sa(sockfd, &mut new);
            fd_table::with_mut(|t| {
                if let Some(e) = t.get_mut(&sockfd) {
                    e.mark_bind_called();
                }
            });
        }

        let fp = OLD_BIND.get().expect("OLD_BIND not initialized");
        fp(sockfd, (&raw const new).cast::<libc::sockaddr>(), addrlen)
    }
}

/// Set the thread-local `errno`. Wraps `__errno_location` (POSIX glibc) so
/// the call site is portable across Linux glibc and Android bionic.
fn set_errno(value: c_int) {
    // Safety: `__errno_location` returns a valid pointer to a thread-local
    // `int`; the write is well-defined.
    unsafe {
        *libc::__errno_location() = value;
    }
}

// =====================================================================
// Shared helpers for the connect-side hooks
// =====================================================================

/// For a fresh socket that hasn't been bound yet, force a `bind(2)` to the
/// forced local address/port. Called before `connect`/`sendto`/`sendmsg`.
///
/// Mirrors the C version's `change_local_binding()`. The flow:
///
///  1. Look up the fd in the table; bail if not a tracked network socket.
///  2. If we've already forced a bind for this fd, bail (avoid double-bind).
///  3. Call `getsockname` to read the kernel-assigned local address.
///  4. Apply `alter_sa` (writes `ADDRESS_*` / `PORT_*`).
///  5. If anything was altered, call the *real* `bind(2)` through the saved
///     `OLD_BIND` pointer (NOT through our own hook — that would recurse).
pub fn change_local_binding(sockfd: c_int) {
    crate::shim::init::init();
    xlog!(2, "change_local_binding(sockfd={})\n", sockfd);

    let info = fd_table::get(sockfd);
    let info = match info {
        Some(i) if i.is_netsock() => i,
        _ => return,
    };
    if info.is_bind_called() {
        return;
    }

    // Read the kernel-assigned local address.
    let mut tmp: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
    let mut tmp_len: socklen_t = SOCKADDR_STORAGE_SIZE as socklen_t;
    let err = unsafe {
        libc::getsockname(
            sockfd,
            (&raw mut tmp).cast::<libc::sockaddr>(),
            &raw mut tmp_len,
        )
    };
    if err != 0 {
        let e = std::io::Error::last_os_error();
        xlog!(
            1,
            "Cannot get socket name err={} ({}) [{}]!\n",
            err,
            e,
            sockfd
        );
        return;
    }

    // Apply forced address/port to the local side.
    if !sockaddr::alter_sa(sockfd, &mut tmp) {
        return;
    }

    // Call the real bind via the saved function pointer (NOT our hook).
    let fp = OLD_BIND.get().expect("OLD_BIND not initialized");
    let err = unsafe { fp(sockfd, (&raw const tmp).cast::<libc::sockaddr>(), tmp_len) };

    fd_table::with_mut(|t| {
        if let Some(e) = t.get_mut(&sockfd) {
            e.mark_bind_called();
        }
    });

    if err != 0 {
        let e = std::io::Error::last_os_error();
        xlog!(1, "Cannot bind err={} ({}) [{}]!\n", err, e, sockfd);
    }
}

/// Copy the (possibly-already-mutated) destination `sockaddr_storage` into
/// the `SocketInfo::dest` buffer for the bandwidth code (step 9) and the
/// IPv6 flowinfo setup (step 8) to read later.
///
/// No-op for untracked fds / non-network sockets.
///
/// Order matters and matches the C version:
///  1. Override the destination's `sin6_flowinfo` if flowinfo is forced.
///  2. Copy the destination into `SocketInfo::dest` so the flowlabel
///     manager install can use it as `flr_dst`.
///  3. Install the flowlabel manager (one-shot per socket).
pub fn alter_dest_sa(sockfd: c_int, ss: &libc::sockaddr_storage, len: socklen_t) {
    crate::shim::init::init();
    let s = sockaddr::format(ss);
    xlog!(2, "alter_dest_sa(sockfd={}, addr={})\n", sockfd, s);

    // The flowinfo override mutates `ss` in place before we copy it.
    // We need a local copy we can mutate, since `ss` is `&` (not `&mut`).
    let mut ss_owned: libc::sockaddr_storage = *ss;
    crate::shim::flowinfo::override_dest_flowinfo(sockfd, &mut ss_owned);

    fd_table::with_mut(|t| {
        let info = match t.get_mut(&sockfd) {
            Some(i) if i.is_netsock() => i,
            _ => return,
        };

        let copy_len = (len as usize).min(SOCKADDR_STORAGE_SIZE);
        let bytes =
            unsafe { std::slice::from_raw_parts((&raw const ss_owned).cast::<u8>(), copy_len) };
        info.dest[..copy_len].copy_from_slice(bytes);
        info.dest_len = len;

        // Install flowlabel manager for v6 sockets (one-shot).
        crate::shim::flowinfo::install_for_socket(sockfd, info);
    });
}

// =====================================================================
// Hook: connect(2)
// =====================================================================

/// `int connect(int sockfd, const struct sockaddr *addr, socklen_t addrlen);`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn connect(
    sockfd: c_int,
    addr: *const libc::sockaddr,
    addrlen: socklen_t,
) -> c_int {
    unsafe {
        crate::shim::init::init();
        xlog!(2, "connect(sockfd={}, ...)\n", sockfd);

        change_local_binding(sockfd);

        let mut new_dest: libc::sockaddr_storage = std::mem::zeroed();
        sockaddr::copy_into(&mut new_dest, addr, addrlen as usize);
        alter_dest_sa(sockfd, &new_dest, addrlen);

        let fp = OLD_CONNECT.get().expect("OLD_CONNECT not initialized");
        fp(
            sockfd,
            (&raw const new_dest).cast::<libc::sockaddr>(),
            addrlen,
        )
    }
}

// =====================================================================
// Hook: sendto(2)
// =====================================================================

/// `ssize_t sendto(int sockfd, const void *buf, size_t len, int flags,
///                 const struct sockaddr *dest_addr, socklen_t addrlen);`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sendto(
    sockfd: c_int,
    buf: *const libc::c_void,
    len: libc::size_t,
    flags: c_int,
    dest_addr: *const libc::sockaddr,
    addrlen: socklen_t,
) -> libc::ssize_t {
    unsafe {
        crate::shim::init::init();
        xlog!(
            1,
            "sendto(sockfd, {}, buf, len={}, flags=0x{:x}, ...)\n",
            sockfd,
            len,
            flags
        );

        change_local_binding(sockfd);

        let mut new_dest: libc::sockaddr_storage = std::mem::zeroed();
        sockaddr::copy_into(&mut new_dest, dest_addr, addrlen as usize);
        alter_dest_sa(sockfd, &new_dest, addrlen);

        let fp = OLD_SENDTO.get().expect("OLD_SENDTO not initialized");
        let n = fp(
            sockfd,
            buf,
            len,
            flags,
            (&raw const new_dest).cast::<libc::sockaddr>(),
            addrlen,
        );

        bw::throttle(sockfd, n.max(0) as usize);
        n
    }
}

// =====================================================================
// Hook: sendmsg(2)
// =====================================================================

/// `ssize_t sendmsg(int sockfd, const struct msghdr *msg, int flags);`
///
/// TODO (parity with C version): the destination in `msg.msg_name` is not
/// rewritten. The C version has the same TODO. To honor this properly we'd
/// need to construct a *new* `msghdr` pointing at our rewritten sockaddr,
/// then call `old_sendmsg` with that — non-trivial since `msg` and its
/// iovecs may be read-only from the kernel's perspective.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn sendmsg(
    sockfd: c_int,
    msg: *const libc::msghdr,
    flags: c_int,
) -> libc::ssize_t {
    unsafe {
        crate::shim::init::init();
        xlog!(1, "sendmsg(sockfd={}, ..., flags=0x{:x})\n", sockfd, flags);

        change_local_binding(sockfd);

        let fp = OLD_SENDMSG.get().expect("OLD_SENDMSG not initialized");
        let n = fp(sockfd, msg, flags);

        bw::throttle(sockfd, n.max(0) as usize);
        n
    }
}

// =====================================================================
// Hook: send(2)
// =====================================================================

/// `ssize_t send(int sockfd, const void *buf, size_t len, int flags);`
#[unsafe(no_mangle)]
pub unsafe extern "C" fn send(
    sockfd: c_int,
    buf: *const libc::c_void,
    len: libc::size_t,
    flags: c_int,
) -> libc::ssize_t {
    unsafe {
        crate::shim::init::init();
        xlog!(
            1,
            "send(sockfd={}, buf, len={}, flags=0x{:x})\n",
            sockfd,
            len,
            flags
        );

        let fp = OLD_SEND.get().expect("OLD_SEND not initialized");
        let n = fp(sockfd, buf, len, flags);

        bw::throttle(sockfd, n.max(0) as usize);
        n
    }
}

// =====================================================================
// Hook: write(2)
// =====================================================================

/// `ssize_t write(int fd, const void *buf, size_t count);`
///
/// Note: `write` is not just for sockets — it can be called on regular
/// files, pipes, etc. We unconditionally call [`bw::throttle`]; the
/// function itself bails out for non-network fds (no entry in the table).
///
/// Reentrance guard: if `xlog!` (which calls `libc::write` internally) is
/// in flight on this thread, we pass through to the original without side
/// effects. Without this, `bw::throttle` would lock the `FD_TABLE` Mutex
/// from inside `libc::write` — which deadlocks against glibc's internal
/// allocator/loader locks.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn write(
    fd: c_int,
    buf: *const libc::c_void,
    len: libc::size_t,
) -> libc::ssize_t {
    unsafe {
        crate::shim::init::init();

        let fp = OLD_WRITE.get().expect("OLD_WRITE not initialized");

        if is_reentrant() {
            // xlog! inside our own code path triggered us; skip side effects
            // (bw::throttle would lock fd_table and deadlock).
            return fp(fd, buf, len);
        }
        let _guard = enter_hook();

        let n = fp(fd, buf, len);
        bw::throttle(fd, n.max(0) as usize);
        n
    }
}

// =====================================================================
// Hook: accept(2)
// =====================================================================

/// `int accept(int sockfd, struct sockaddr *addr, socklen_t *addrlen);`
///
/// Mirrors the C version (which also restarts on EINTR — that was added in
/// the most recent upstream commit). After a successful accept, we run
/// [`socket_create_callback`] on the new fd, inheriting the listening
/// socket's `domain` and `type_` so forced options apply to accepted
/// connections too.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn accept(
    sockfd: c_int,
    addr: *mut libc::sockaddr,
    addrlen: *mut socklen_t,
) -> c_int {
    crate::shim::init::init();
    xlog!(2, "accept(sockfd={}, ...)\n", sockfd);

    let fp = OLD_ACCEPT.get().expect("OLD_ACCEPT not initialized");

    // EINTR restart loop — matches the upstream commit
    // "Restart accept interrupted by signal".
    let new_sock = loop {
        let r = unsafe { fp(sockfd, addr, addrlen) };
        if r == -1 && unsafe { *libc::__errno_location() } == libc::EINTR {
            continue;
        }
        break r;
    };

    if new_sock == -1 {
        return -1;
    }

    // Inherit the listening socket's domain/type so forced options apply.
    if let Some(info) = fd_table::get(sockfd) {
        socket_create_callback(new_sock, info.domain, info.type_);
    }

    new_sock
}

// =====================================================================
// Hook: poll(2)
// =====================================================================

/// `int poll(struct pollfd *fds, nfds_t nfds, int timeout);`
///
/// Optionally overrides `timeout` with `POLL_TIMEOUT` if set.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn poll(fds: *mut libc::pollfd, nfds: libc::nfds_t, timeout: c_int) -> c_int {
    unsafe {
        crate::shim::init::init();
        let cfg = crate::shim::init::init();
        xlog!(
            2,
            "poll(fds, {}, {}) old_poll={:?}\n",
            nfds,
            timeout,
            OLD_POLL.get().map(|p| *p as *const ())
        );

        let effective_timeout = match cfg.force_poll_timeout {
            Some(t) => t,
            None => timeout,
        };

        let fp = OLD_POLL.get().expect("OLD_POLL not initialized");
        fp(fds, nfds, effective_timeout)
    }
}

// =====================================================================
// Hook: setsockopt(2)
// =====================================================================

/// `int setsockopt(int sockfd, int level, int optname, const void *optval, socklen_t optlen);`
///
/// When the `(level, optname)` pair is one our intercept table owns, return
/// the forced value without forwarding to libc; otherwise pass through.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn setsockopt(
    sockfd: c_int,
    level: c_int,
    optname: c_int,
    optval: *const libc::c_void,
    optlen: socklen_t,
) -> c_int {
    unsafe {
        crate::shim::init::init();

        if let Some(ret) = crate::shim::setsockopt::intercept(sockfd, level, optname) {
            return ret;
        }

        let fp = OLD_SETSOCKOPT
            .get()
            .expect("OLD_SETSOCKOPT not initialized");
        fp(sockfd, level, optname, optval, optlen)
    }
}
