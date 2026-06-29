//! rbind — an `LD_PRELOAD` shim that forces bind/connect socket options
//! based on environment variables.
//!
//! This crate is a `cdylib`; the build artifact is `target/release/librbind.so`.
//! Copy or symlink to `rbind.so` for the un-prefixed form typically used with
//! `LD_PRELOAD`.
//!
//! Layout:
//!   - [`bw`]          — bandwidth throttling (token bucket + nanosleep)
//!   - [`config`]      — `Config` struct + env-var loader
//!   - [`consts`]      — local shims for libc constants/types not guaranteed by `libc`
//!   - [`fd_table`]    — process-wide `Mutex<HashMap<RawFd, SocketInfo>>`
//!   - [`flowinfo`]    — IPv6 flowinfo override + flowlabel manager install
//!   - [`hooks`]       — `#[no_mangle]` exported libc hooks
//!   - [`init`]        — one-shot `init()` (OnceLock-guarded)
//!   - [`log`]         — `Mutex<File>` logger + `xlog!` macro
//!   - [`setsockopt`]  — forced-options intercept table
//!   - [`sockaddr`]    — `sockaddr_storage` views + format + alter
//!   - [`socket`]      — `SocketInfo` struct + flag constants
//!   - [`syscalls`]    — fn-pointer typedefs + `dlsym(RTLD_NEXT, ...)` resolvers

#![allow(non_camel_case_types)]

pub mod bw;
pub mod config;
pub mod consts;
pub mod fd_table;
pub mod flowinfo;
pub mod hooks;
pub mod init;
pub mod log;
pub mod setsockopt;
pub mod sockaddr;
pub mod socket;
pub mod syscalls;

// Note on the missing `.init_array` constructor:
// We deliberately do NOT add one. Calling `init()` (which opens a log file
// and acquires mutexes) from `.init_array` can deadlock on glibc because
// the pthread subsystem isn't fully initialized at that point — confirmed
// empirically: a `writeln!` to the log file hangs forever. Instead we
// mirror the C version: `init()` is called lazily from each hook's first
// invocation, guarded by `OnceLock` so it runs exactly once.

// Hook symbols (`bind`, `socket`, …) and the rest of the modules land here
// as the rewrite progresses through steps 3-11 of the implementation plan.
