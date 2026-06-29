//! One-shot process initialization.
//!
//! Mirrors the C version's `init()`: lazy (called from each hook), guarded
//! by a [`OnceLock`] so it runs exactly once per process.

#![allow(dead_code)]

use std::sync::OnceLock;

use crate::shim::{config::Config, log, syscalls};

static CONFIG: OnceLock<Config> = OnceLock::new();

/// Run init exactly once and return the populated [`Config`]. Subsequent
/// calls return the cached value (and do no work).
pub fn init() -> &'static Config {
    CONFIG.get_or_init(do_init)
}

fn do_init() -> Config {
    // ---- Open log file ----
    if let Ok(path) = std::env::var("LOG")
        && let Err(_e) = log::open(&path)
    {
        // No log file is open yet, so we can't xlog! this. Just swallow
        // it; the C version silently fails here too.
    }

    // ---- Verbose ----
    if let Ok(s) = std::env::var("VERBOSE")
        && let Some(n) = crate::shim::config::parse_int_base0(&s)
    {
        log::set_verbose(n as u32);
    }

    // ---- Load & log every FORCE_* env var ----
    let cfg = Config::load_from_env();

    // ---- Initialize bandwidth throttle ----
    crate::shim::bw::set_global_limit(cfg.bw_limit_global);

    // ---- Hijack libc symbols ----
    syscalls::resolve_all();

    cfg
}
