//! Locate `librbind.so` and `exec()` the target program with `LD_PRELOAD`
//! pointing at it plus the merged env from profile+flags.

#![allow(dead_code)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::process::Command;

use super::config;

/// Locate the shim's `.so`. Search order:
///
/// 1. `$RBIND_SO_PATH` — escape hatch for testing or non-standard install
///    locations. Always wins over the config file when set.
/// 2. `so_path` from `~/.config/rbind/config.toml` — the user-level way to
///    point rbind at a custom install without walking the filesystem. A
///    missing or unreadable file is silently treated as "unset".
/// 3. Next to the running `rbind` binary (`./librbind.so` then `./rbind.so`).
/// 4. System-wide install paths (`/usr/local/lib/librbind.so`, `/usr/lib/librbind.so`).
pub fn find_so() -> anyhow::Result<PathBuf> {
    if let Ok(p) = std::env::var("RBIND_SO_PATH") {
        let path = expand_home(&p);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("RBIND_SO_PATH={} does not exist", path.display());
    }

    if let Some(p) = config::load().so_path {
        let path = expand_home(&p);
        if path.exists() {
            return Ok(path);
        }
        anyhow::bail!("config.toml so_path={} does not exist", path.display());
    }

    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        // The current artifact name is `librbind_lib.so`. The previous
        // (pre-rename) name `librbind.so` is checked second for users
        // with stale build artifacts; the un-prefixed `rbind.so` is
        // accepted as a courtesy for hand-symlinked installs.
        for name in ["librbind_lib.so", "librbind.so", "rbind.so"] {
            let candidate = dir.join(name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
    }

    for path in [
        "/usr/local/lib/librbind_lib.so",
        "/usr/lib/librbind_lib.so",
        "/usr/local/lib/librbind.so", // legacy pre-rename install path
        "/usr/lib/librbind.so",
    ] {
        let p = PathBuf::from(path);
        if p.exists() {
            return Ok(p);
        }
    }

    anyhow::bail!(
        "could not find librbind.so. Set RBIND_SO_PATH, or create \
         ~/.config/rbind/config.toml with:\n  \
         so_path = \"/path/to/librbind.so\"  # ~ is also accepted\n\
         (or place the .so next to the rbind binary / in /usr/local/lib)"
    )
}

/// Expand a leading `~` to the current user's home directory.
///
/// Only the two POSIX shell forms are supported:
/// - `~`          → `$HOME`
/// - `~/...`      → `$HOME/...`
///
/// `~user/...` is intentionally *not* handled (no portable lookup on Linux
/// without an NSS call). Anything else is returned unchanged.
fn expand_home(p: &str) -> PathBuf {
    if p == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = p.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(p)
}

#[cfg(test)]
mod tests {
    use super::expand_home;

    #[test]
    fn expands_tilde_slash() {
        let p = expand_home("~/.cargo/bin/foo");
        assert!(p.ends_with(".cargo/bin/foo"));
        assert!(!p.to_string_lossy().contains('~'));
    }

    #[test]
    fn expands_lone_tilde() {
        let p = expand_home("~");
        assert!(!p.to_string_lossy().contains('~'));
        assert!(!p.as_os_str().is_empty());
    }

    #[test]
    fn leaves_absolute_path_alone() {
        let p = expand_home("/usr/local/lib/librbind_lib.so");
        assert_eq!(p.to_string_lossy(), "/usr/local/lib/librbind_lib.so");
    }

    #[test]
    fn leaves_other_user_unchanged() {
        // `~user/...` is intentionally not expanded.
        let p = expand_home("~bob/foo");
        assert_eq!(p.to_string_lossy(), "~bob/foo");
    }
}

/// Prepend the resolved `.so` to `LD_PRELOAD` and `exec()` the target command
/// with the merged env. This replaces the current process image, so signals
/// and exit codes behave as if the user had run the program directly.
pub fn exec_with_env<S: ::std::hash::BuildHasher>(
    so_path: &std::path::Path,
    env: &HashMap<String, String, S>,
    cmd: &[String],
) -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;

    if cmd.is_empty() {
        anyhow::bail!("no command given — usage: rbind run [flags] -- <command> [args...]");
    }

    let mut command = Command::new(&cmd[0]);
    command.args(&cmd[1..]);

    // Preserve any pre-existing LD_PRELOAD and append our .so.
    let existing = std::env::var("LD_PRELOAD").unwrap_or_default();
    let new_preload = if existing.is_empty() {
        so_path.display().to_string()
    } else {
        format!("{}:{}", so_path.display(), existing)
    };
    command.env("LD_PRELOAD", new_preload);

    for (k, v) in env {
        command.env(k, v);
    }

    // CommandExt::exec replaces this process image with the target.
    let err = command.exec();
    // If exec() returns, it failed.
    Err(anyhow::anyhow!("failed to exec '{}': {err}", cmd[0]))
}
