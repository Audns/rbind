//! Function-pointer typedefs and `dlsym(RTLD_NEXT, ...)` resolvers for the
//! libc symbols we hijack via `LD_PRELOAD`.
//!
//! ## Why function pointers and not direct `extern "C"` imports?
//!
//! If we declared `extern "C" { fn bind(...) -> ...; }` and called it, the
//! dynamic linker would resolve the import to the very same `bind` symbol
//! that our `#[no_mangle] pub unsafe extern "C" fn bind(...)` exports —
//! i.e. recursion into ourselves. The fix, exactly as the C version does,
//! is to resolve the real implementation at runtime via `dlsym(RTLD_NEXT, ...)`
//! and store it in a function pointer. Then we call through the pointer.
//!
//! The pointers are stored in `OnceLock`s so they are initialized exactly once
//! (typically from `init()`) and are safe to read from any hook thereafter.

#![allow(non_camel_case_types, dead_code)]

use std::sync::OnceLock;

use libc::{c_int, c_void, nfds_t, size_t, socklen_t, ssize_t};

// =====================================================================
// Original libc function pointer types
// =====================================================================

/// `int bind(int sockfd, const struct sockaddr *addr, socklen_t addrlen);`
pub type FnBind = unsafe extern "C" fn(c_int, *const libc::sockaddr, socklen_t) -> c_int;

/// `int setsockopt(int sockfd, int level, int optname, const void *optval, socklen_t optlen);`
pub type FnSetsockopt =
    unsafe extern "C" fn(c_int, c_int, c_int, *const c_void, socklen_t) -> c_int;

/// `int socket(int domain, int type, int protocol);`
pub type FnSocket = unsafe extern "C" fn(c_int, c_int, c_int) -> c_int;

/// `int close(int fd);`
pub type FnClose = unsafe extern "C" fn(c_int) -> c_int;

/// `ssize_t write(int fd, const void *buf, size_t count);`
pub type FnWrite = unsafe extern "C" fn(c_int, *const c_void, size_t) -> ssize_t;

/// `ssize_t send(int sockfd, const void *buf, size_t len, int flags);`
pub type FnSend = unsafe extern "C" fn(c_int, *const c_void, size_t, c_int) -> ssize_t;

/// `ssize_t sendto(int sockfd, const void *buf, size_t len, int flags,
///                 const struct sockaddr *dest_addr, socklen_t addrlen);`
pub type FnSendto = unsafe extern "C" fn(
    c_int,
    *const c_void,
    size_t,
    c_int,
    *const libc::sockaddr,
    socklen_t,
) -> ssize_t;

/// `ssize_t sendmsg(int sockfd, const struct msghdr *msg, int flags);`
pub type FnSendmsg = unsafe extern "C" fn(c_int, *const libc::msghdr, c_int) -> ssize_t;

/// `int accept(int sockfd, struct sockaddr *addr, socklen_t *addrlen);`
pub type FnAccept = unsafe extern "C" fn(c_int, *mut libc::sockaddr, *mut socklen_t) -> c_int;

/// `int connect(int sockfd, const struct sockaddr *addr, socklen_t addrlen);`
pub type FnConnect = unsafe extern "C" fn(c_int, *const libc::sockaddr, socklen_t) -> c_int;

/// `int poll(struct pollfd *fds, nfds_t nfds, int timeout);`
pub type FnPoll = unsafe extern "C" fn(*mut libc::pollfd, nfds_t, c_int) -> c_int;

// =====================================================================
// Resolve a single symbol via dlsym(RTLD_NEXT, ...)
// =====================================================================

/// Resolve `name` via `dlsym(RTLD_NEXT, ...)` and reinterpret the result as
/// a function pointer of type `F`. Aborts the process if the symbol cannot
/// be found (mirrors the C version's `exit(1)` on missing dlsym results).
///
/// # Safety
///
/// The caller must ensure `F` is the correct C-ABI function-pointer type for
/// the symbol named by `name`. Reinterpreting a function pointer as the
/// wrong type and calling it is undefined behavior.
pub unsafe fn resolve<F: Copy>(name: &'static core::ffi::CStr) -> F {
    unsafe {
        let sym = libc::dlsym(libc::RTLD_NEXT, name.as_ptr());
        if sym.is_null() {
            // Match C version: bail out hard. `abort()` is signal-safe, so it's
            // safe to call from an `LD_PRELOAD` context where the host process
            // may have hooked `exit`/`atexit`.
            std::process::abort();
        }
        // Both `*mut c_void` and a fn pointer are pointer-sized on all supported
        // platforms (8 bytes on 64-bit Linux). `transmute_copy` performs a
        // same-size bit copy, which is what we want here.
        std::mem::transmute_copy::<*mut c_void, F>(&sym)
    }
}

// =====================================================================
// OnceLock-initialized function pointers, populated by resolve_all()
// =====================================================================

pub static OLD_BIND: OnceLock<FnBind> = OnceLock::new();
pub static OLD_SETSOCKOPT: OnceLock<FnSetsockopt> = OnceLock::new();
pub static OLD_SOCKET: OnceLock<FnSocket> = OnceLock::new();
pub static OLD_CLOSE: OnceLock<FnClose> = OnceLock::new();
pub static OLD_WRITE: OnceLock<FnWrite> = OnceLock::new();
pub static OLD_SEND: OnceLock<FnSend> = OnceLock::new();
pub static OLD_SENDTO: OnceLock<FnSendto> = OnceLock::new();
pub static OLD_SENDMSG: OnceLock<FnSendmsg> = OnceLock::new();
pub static OLD_ACCEPT: OnceLock<FnAccept> = OnceLock::new();
pub static OLD_CONNECT: OnceLock<FnConnect> = OnceLock::new();
pub static OLD_POLL: OnceLock<FnPoll> = OnceLock::new();

/// Resolve every libc symbol we'll hijack and store it in its `OnceLock`.
/// Idempotent — safe to call multiple times; each `OnceLock::get_or_init`
/// resolves at most once across the lifetime of the process.
pub fn resolve_all() {
    OLD_BIND.get_or_init(|| unsafe { resolve::<FnBind>(c"bind") });
    OLD_SETSOCKOPT.get_or_init(|| unsafe { resolve::<FnSetsockopt>(c"setsockopt") });
    OLD_SOCKET.get_or_init(|| unsafe { resolve::<FnSocket>(c"socket") });
    OLD_CLOSE.get_or_init(|| unsafe { resolve::<FnClose>(c"close") });
    OLD_WRITE.get_or_init(|| unsafe { resolve::<FnWrite>(c"write") });
    OLD_SEND.get_or_init(|| unsafe { resolve::<FnSend>(c"send") });
    OLD_SENDTO.get_or_init(|| unsafe { resolve::<FnSendto>(c"sendto") });
    OLD_SENDMSG.get_or_init(|| unsafe { resolve::<FnSendmsg>(c"sendmsg") });
    OLD_ACCEPT.get_or_init(|| unsafe { resolve::<FnAccept>(c"accept") });
    OLD_CONNECT.get_or_init(|| unsafe { resolve::<FnConnect>(c"connect") });
    OLD_POLL.get_or_init(|| unsafe { resolve::<FnPoll>(c"poll") });
}
