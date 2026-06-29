//! User-level app config (`~/.config/rbind/config.toml`).
//!
//! Currently only holds a single field — `so_path` — used by
//! [`super::launch::find_so`] to short-circuit the directory walk when the
//! user has installed the shim somewhere non-standard. The struct is shaped
//! as a flat top-level table so future keys (`log_dir`, default profile, …)
//! can be added without a breaking schema change.

#![allow(dead_code)]

use serde::Deserialize;
use std::path::PathBuf;

/// In-memory representation of `~/.config/rbind/config.toml`. Each field is
/// `Option<T>` so an absent or partial file is a valid "use defaults" state.
#[derive(Deserialize, Debug, Default)]
pub struct AppConfig {
    /// Absolute or `~`-relative path to `librbind.so`. When set, the launcher's
    /// directory walk is skipped and this path is used instead.
    pub so_path: Option<String>,
}

/// Path to the user's config file. Returns `None` only if the OS has no
/// config dir at all (vanishingly rare on Linux). Mirrors
/// [`super::profile::profile_path`].
#[must_use]
pub fn config_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rbind").join("config.toml"))
}

/// Read the config file. A missing file is *not* an error — that's the
/// default state on a fresh install.
#[must_use]
pub fn load() -> AppConfig {
    let Some(path) = config_path() else {
        return AppConfig::default();
    };
    if !path.exists() {
        return AppConfig::default();
    }
    match std::fs::read_to_string(&path) {
        Err(e) => {
            eprintln!("rbind: warning: reading {}: {e}", path.display());
            AppConfig::default()
        }
        Ok(text) => match toml::from_str::<AppConfig>(&text) {
            Err(e) => {
                eprintln!("rbind: warning: parsing {}: {e}", path.display());
                AppConfig::default()
            }
            Ok(cfg) => cfg,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_empty() {
        let cfg = AppConfig::default();
        assert!(cfg.so_path.is_none());
    }

    #[test]
    fn parses_so_path() {
        let cfg: AppConfig = toml::from_str(r#"so_path = "/opt/rbind/librbind.so""#).unwrap();
        assert_eq!(cfg.so_path.as_deref(), Some("/opt/rbind/librbind.so"));
    }

    #[test]
    fn empty_file_parses_as_default() {
        let cfg: AppConfig = toml::from_str("").unwrap();
        assert!(cfg.so_path.is_none());
    }
}
