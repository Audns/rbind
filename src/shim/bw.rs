//! Bandwidth throttling — token-bucket algorithm with `nanosleep`.
//!
//! Two modes (configured via env at process init):
//! - **Global**: `BW=N` bytes/sec across all sockets
//! - **Per-socket**: `BW_PER_SOCKET=N` bytes/sec per socket
//!
//! The two modes are mutually exclusive — the C version's `init()` refuses
//! to set per-socket when global is also set; we do the same in
//! [`crate::config::Config::load_from_env`].
//!
//! ## Algorithm
//!
//! For each call to [`throttle`] with `bytes`:
//!
//!  1. Look up the socket's effective rate (`info.limit` if non-zero,
//!     else `BW_GLOBAL.limit`).
//!  2. Compute `diff_ms = now - last` (monotonic via [`Instant`]).
//!  3. `allowed = rest + limit * diff_ms / 1000`.
//!  4. If `bytes <= allowed`: consume tokens, return.
//!  5. Else: sleep for `(bytes - allowed) * 1000 / limit` ms, advancing
//!     `last` by that amount *before* sleeping so the next call doesn't
//!     credit the wall-clock time we spent sleeping.
//!
//! ## Signal safety
//!
//! `nanosleep` is interrupted by signals (EINTR). We loop, restarting with
//! the kernel's reported remaining time. `std::thread::sleep` is NOT
//! signal-safe in the way an `LD_PRELOAD` shim needs it to be, so we use
//! libc's nanosleep directly.

#![allow(dead_code)]

use std::os::unix::io::RawFd;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use crate::shim::fd_table;
use crate::xlog;

/// State for the global bandwidth limit (used when a socket has no per-socket
/// limit). Persists across calls so the token bucket refills correctly.
struct BwState {
    limit: u64,
    rest: u64,
    last: Option<Instant>,
}

impl BwState {
    fn new() -> Self {
        Self {
            limit: 0,
            rest: 0,
            last: None,
        }
    }
}

static BW_GLOBAL: LazyLock<Mutex<BwState>> = LazyLock::new(|| Mutex::new(BwState::new()));

/// Set the global rate limit (called from `init()` after reading
/// `BW`). `limit` is in bytes/sec; `0` disables global throttling.
pub fn set_global_limit(limit: u64) {
    let mut g = BW_GLOBAL
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if limit > 0 && g.limit == 0 {
        g.limit = limit;
        g.rest = 0;
        g.last = Some(Instant::now());
    }
}

/// Throttle after a write-like syscall. `bytes` is the count actually
/// written (may be `-1` cast to `usize` on error; we treat 0 as no-op).
pub fn throttle(sockfd: RawFd, bytes: usize) {
    xlog!(2, "bw(sockfd={}, bytes={})\n", sockfd, bytes);
    if bytes == 0 {
        return;
    }

    // Look up the fd; bail if not tracked or not a network socket.
    let info = match fd_table::get(sockfd) {
        Some(i) if i.is_netsock() => i,
        _ => return,
    };

    // Determine which limit to use.
    let (limit, rest, last) = if info.limit > 0 {
        (
            info.limit,
            info.rest,
            info.last.unwrap_or_else(Instant::now),
        )
    } else {
        let cfg = crate::shim::init::init();
        if cfg.bw_limit_global == 0 {
            return;
        }
        let g = BW_GLOBAL
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if g.limit == 0 {
            return;
        }
        (g.limit, g.rest, g.last.unwrap_or_else(Instant::now))
    };

    let now = Instant::now();
    // Saturating: Instant panics on negative duration_since, so use
    // checked_sub and treat negative as 0.
    let diff_ms = now.saturating_duration_since(last).as_millis() as u64;

    let allowed = rest.saturating_add(limit.saturating_mul(diff_ms) / 1000);
    let bytes_u64 = bytes as u64;

    if bytes_u64 <= allowed {
        // Fast path: enough tokens, no sleep needed.
        let new_rest = allowed - bytes_u64;
        write_back(sockfd, new_rest, now);
        return;
    }

    // Slow path: need to sleep, then advance `last` past the sleep.
    let sleep_ms = bytes_u64.saturating_sub(allowed).saturating_mul(1000) / limit;
    let new_last = now + Duration::from_millis(sleep_ms);

    // Write back before sleeping — holds the fd_table mutex only briefly.
    write_back(sockfd, 0, new_last);

    xlog!(2, "sleeping {}ms for bandwidth\n", sleep_ms);
    nanosleep_ms(sleep_ms);
}

/// Update `rest` and `last` for a socket (no-op if the fd is no longer
/// tracked, e.g. closed during the throttle).
fn write_back(sockfd: RawFd, rest: u64, last: Instant) {
    fd_table::with_mut(|t| {
        if let Some(info) = t.get_mut(&sockfd) {
            info.rest = rest;
            info.last = Some(last);
        }
    });
}

/// Sleep for `ms` milliseconds via `nanosleep(2)`, restarting on EINTR.
fn nanosleep_ms(ms: u64) {
    let mut ts = libc::timespec {
        tv_sec: (ms / 1000).cast_signed(),
        tv_nsec: ((ms % 1000) * 1_000_000).cast_signed(),
    };
    loop {
        let mut rest: libc::timespec = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        let r = unsafe { libc::nanosleep(&raw const ts, &raw mut rest) };
        if r == 0 {
            break;
        }
        let errno = std::io::Error::last_os_error();
        if errno.raw_os_error() == Some(libc::EINTR) {
            // Interrupted by a signal — sleep the remaining time.
            ts = rest;
            continue;
        }
        xlog!(1, "nanosleep returned error ({}).\n", errno);
        break;
    }
}
