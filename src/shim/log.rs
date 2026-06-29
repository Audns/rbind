//! Logger for rbind.
//!
//! Mirrors the C version's `xlog()` semantics:
//! - silently drops messages if no log file is open
//! - silently drops messages above the verbose threshold
//! - one line per call
//!
//! ## Why an atomic fd, not a `Mutex<Option<File>>`?
//!
//! We tried the obvious `Mutex<Option<File>>` first. It deadlocked when
//! `log::open` was called from inside an `LD_PRELOAD` hook (e.g. via the
//! lazy `init()` triggered by `socket()`). The root cause: `Mutex::lock()`
//! on Linux goes through `pthread_mutex_lock`, which can reenter glibc's
//! internal allocator/loader locks. When the host process is mid-libc-call
//! (which is *always* true inside an `LD_PRELOAD` hook), reentrance of those
//! internal locks can deadlock.
//!
//! `AtomicI32` sidesteps the issue entirely — `load`/`store` are
//! lock-free and never touch glibc's allocator. Concurrent log writes may
//! interleave bytes across threads, which is acceptable for human-readable
//! debug output.
//!
//! This pattern is what production `LD_PRELOAD` shims (ltrace, libseccomp
//! wrappers) use.

#![allow(dead_code)]

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

/// Raw file descriptor for the log file. `-1` = no log open. Initialized
/// lazily on first use via the const initializer.
static LOG_FD: AtomicI32 = AtomicI32::new(-1);
static VERBOSE: AtomicU32 = AtomicU32::new(0);

/// Open (truncate, write, create) the log file at `path`. Called from
/// `init()` after reading `LOG`. Failures are returned to the
/// caller; the `init()` path swallows them (matching the C version).
pub fn open(path: &str) -> std::io::Result<()> {
    let path_cstr = std::ffi::CString::new(path).map_err(|_| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has interior NUL")
    })?;
    let fd = unsafe {
        libc::open(
            path_cstr.as_ptr(),
            libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC,
            0o644,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error());
    }
    LOG_FD.store(fd, Ordering::Release);
    Ok(())
}

/// Set the verbosity threshold. Messages with `level > verbose()` are dropped.
pub fn set_verbose(level: u32) {
    VERBOSE.store(level, Ordering::Relaxed);
}

/// Read the current verbosity threshold.
pub fn verbose() -> u32 {
    VERBOSE.load(Ordering::Relaxed)
}

/// Write one log line at `level` if a log file is open and the level passes
/// the verbose threshold. No-op otherwise.
pub fn write(level: u32, args: std::fmt::Arguments<'_>) {
    if level > verbose() {
        return;
    }
    let fd = LOG_FD.load(Ordering::Acquire);
    if fd < 0 {
        return;
    }

    // Format into a stack buffer. 1024 bytes is plenty for our log lines;
    // longer messages get truncated (matches the C version's 128-byte tmp
    // buffer behavior).
    let mut buf = [0u8; 1024];
    let s = args.to_string();
    let bytes = s.as_bytes();
    let n = bytes.len().min(buf.len() - 1);
    buf[..n].copy_from_slice(&bytes[..n]);
    buf[n] = b'\n';
    let to_write = &buf[..=n];

    // Best-effort write; we deliberately swallow errors because the
    // alternative is to abort the host process from a logging failure.
    let _ = unsafe { libc::write(fd, to_write.as_ptr().cast::<libc::c_void>(), to_write.len()) };
}

/// `xlog!(2, "foo={} bar={}\n", foo, bar)` — gated by `verbose()`, just like
/// the C version. The trailing newline (if any) is the caller's responsibility,
/// matching the C `printf`-style usage.
#[macro_export]
macro_rules! xlog {
    ($level:expr, $($arg:tt)*) => {
        $crate::shim::log::write($level, std::format_args!($($arg)*))
    };
    ($level:expr) => {
        $crate::log::write($level, std::format_args!(""))
    };
}
