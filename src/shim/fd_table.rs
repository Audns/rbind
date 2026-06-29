//! Process-wide fd table.
//!
//! Replaces the C version's singly-linked `struct node` list with an
//! `O(1)` `HashMap`. The C version reuses nodes by setting `fd == -1` after
//! `close`; with a `HashMap` we just `remove()` the entry — simpler, and we
//! no longer leak node memory on close.
//!
//! All access goes through the [`FD_TABLE`] static. Helpers in this module
//! ([`add`], [`get`], [`contains`], [`del`], [`with_mut`]) take the lock
//! briefly and return; compound mutations should use [`with_mut`].
//!
//! The map itself is created in a `const` context — no `OnceLock`, no
//! lazy-init dance. Empty from process start, populated on first socket.

#![allow(dead_code)]

use std::collections::HashMap;
use std::os::unix::io::RawFd;
use std::sync::{LazyLock, Mutex};

use crate::shim::socket::SocketInfo;

static FD_TABLE: LazyLock<Mutex<HashMap<RawFd, SocketInfo>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn lock() -> std::sync::MutexGuard<'static, HashMap<RawFd, SocketInfo>> {
    // Recover from poisoning rather than panicking. A poisoned mutex means
    // *some* thread panicked while holding the lock, but the data itself is
    // almost certainly still consistent (our hooks only do short inserts/
    // lookups). For an LD_PRELOAD shim, panicking inside a hook would kill
    // the host process — recovering is strictly better.
    FD_TABLE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Insert (or overwrite) the entry for `fd`.
pub fn add(fd: RawFd, info: SocketInfo) {
    lock().insert(fd, info);
}

/// Look up the entry for `fd`. Returns a clone so the caller doesn't hold
/// the lock across its work.
#[must_use]
pub fn get(fd: RawFd) -> Option<SocketInfo> {
    lock().get(&fd).cloned()
}

/// Cheap existence check.
#[must_use]
pub fn contains(fd: RawFd) -> bool {
    lock().contains_key(&fd)
}

/// Remove the entry for `fd`. Returns `true` if an entry was actually removed.
#[must_use]
pub fn del(fd: RawFd) -> bool {
    lock().remove(&fd).is_some()
}

/// Apply a closure to the table under the lock. Use for compound mutations
/// (e.g. "look up the entry, update one field, write back"). The closure
/// should be quick — every other hook waits while it runs.
pub fn with_mut<R>(f: impl FnOnce(&mut HashMap<RawFd, SocketInfo>) -> R) -> R {
    f(&mut lock())
}

/// Drop every entry. Currently unused; useful for tests and (potentially) for
/// a future "reset on SIGHUP" feature.
pub fn clear() {
    lock().clear();
}

#[cfg(test)]
mod tests {
    //! All tests live in one function because the table is a process-global
    //! and parallel tests would race on `clear()` / shared fd values.

    use super::*;
    use crate::shim::socket::SocketInfo;
    use libc::{AF_INET, AF_INET6, SOCK_DGRAM, SOCK_STREAM};

    #[test]
    fn all_fd_table_tests() {
        clear();

        // ---- add / get / contains / del ----
        let fd: RawFd = 1000;
        assert!(!contains(fd));
        add(fd, SocketInfo::new(AF_INET, SOCK_STREAM));
        assert!(contains(fd));
        let info = get(fd).expect("entry should exist after add");
        assert_eq!(info.domain, AF_INET);
        assert_eq!(info.type_, SOCK_STREAM);
        assert!(del(fd));
        assert!(!contains(fd));
        assert!(get(fd).is_none());
        assert!(!del(fd), "second del should return false");

        // ---- with_mut updates entry ----
        let fd: RawFd = 1001;
        add(fd, SocketInfo::new(AF_INET6, SOCK_DGRAM));
        with_mut(|t| {
            let entry = t.get_mut(&fd).unwrap();
            entry.mark_bind_called();
        });
        let info = get(fd).unwrap();
        assert!(info.is_bind_called());

        // ---- overwrite replaces ----
        let fd: RawFd = 1002;
        add(fd, SocketInfo::new(AF_INET, SOCK_STREAM));
        add(fd, SocketInfo::new(AF_INET6, SOCK_DGRAM));
        let info = get(fd).unwrap();
        assert_eq!(info.domain, AF_INET6);
        assert_eq!(info.type_, SOCK_DGRAM);

        clear();
    }
}
