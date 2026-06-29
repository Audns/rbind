//! Parsed configuration from environment variables.
//!
//! Each field is `Option<T>` where `None` means "no override configured".
//! The C version uses a separate `force_xxx` flag plus a value; we fold both
//! into a single `Option`.
//!
//! Verbosity lives in [`crate::log`], not here, because it is purely a logging
//! concern.

#![allow(dead_code)]

/// Process-wide configuration parsed once at init from the environment.
///
/// Each `force_*` field corresponds to an env var. The names match the
/// short forms used by the `rbind` CLI flag names (e.g. `--bind-device` →
/// `BIND_DEVICE`), so profile entries in `~/.config/rbind/profiles.toml`
/// can use the same key as the flag:
/// - bind address / port  → `ADDRESS_V4`, `ADDRESS_V6`, `PORT_V4`, `PORT_V6`
/// - forced socket options → `BIND_DEVICE`, `TOS`, `TTL`, `KA`, `MSS`,
///   `REUSEADDR`, `NODELAY`, `FLOWINFO`, `FWMARK`, `PRIO`
/// - bandwidth throttling → `BW`, `BW_PER_SOCKET`
/// - poll timeout override → `POLL_TIMEOUT`
/// - debug → `VERBOSE`, `LOG`
#[derive(Default)]
pub struct Config {
    // Forced bind addresses.
    pub force_address_v4: Option<String>,
    pub force_address_v6: Option<String>,

    // Forced bind ports. `None` = leave the port the caller chose.
    pub force_port_v4: Option<u16>,
    pub force_port_v6: Option<u16>,

    // Forced socket options.
    pub force_tos: Option<u8>,
    pub force_ttl: Option<u8>,
    pub force_keepalive: Option<u32>,
    pub force_mss: Option<u32>,
    pub force_reuseaddr: Option<u32>,
    pub force_nodelay: Option<u32>,
    pub force_flowinfo: Option<u32>,
    pub force_fwmark: Option<u32>,
    pub force_prio: Option<u32>,
    /// Network interface to bind all sockets to, e.g. `"eth0"`, `"wg0"`.
    /// Requires `CAP_NET_RAW` or root; silently fails if not permitted.
    pub force_bind_to_device: Option<String>,

    // Bandwidth throttling (units: bytes/second; 0 means "no limit").
    pub bw_limit_global: u64,
    pub bw_limit_per_socket: u64,

    // `poll(2)` timeout override. `None` = leave the caller's timeout alone.
    // The C version uses `-1000` as the unset sentinel; we use `None`.
    pub force_poll_timeout: Option<i32>,
}

impl Config {
    /// Read every `FORCE_*` env var and return the populated `Config`.
    /// Called from [`crate::init::init`].
    ///
    /// **NB:** unlike the C version, we do NOT log each env var here. Writing
    /// to the log from inside `init()` (which is called lazily from the first
    /// hook) was deadlocking against glibc's internal allocator/loader locks.
    /// The forced actions still appear in subsequent hook log lines (e.g.
    /// `bind(sockfd=4, IPv4/127.0.0.2/900)`), so the env-var effect is
    /// observable in the log without this redundant per-var line.
    #[must_use]
    pub fn load_from_env() -> Self {
        let mut cfg = Config::default();

        // ---- Forced bind address ----
        if let Ok(s) = std::env::var("ADDRESS_V4") {
            cfg.force_address_v4 = Some(s);
        }
        if let Ok(s) = std::env::var("ADDRESS_V6") {
            cfg.force_address_v6 = Some(s);
        }

        // ---- Forced bind port ----
        if let Some(n) = load_env_u64("PORT_V4")
            && let Ok(p) = u16::try_from(n)
        {
            cfg.force_port_v4 = Some(p);
        }
        if let Some(n) = load_env_u64("PORT_V6")
            && let Ok(p) = u16::try_from(n)
        {
            cfg.force_port_v6 = Some(p);
        }

        // ---- Forced socket options ----
        if let Some(n) = load_env_u64("TOS") {
            cfg.force_tos = Some(n as u8);
        }
        if let Some(n) = load_env_u64("TTL") {
            cfg.force_ttl = Some(n as u8);
        }
        if let Some(n) = load_env_u64("KA") {
            cfg.force_keepalive = Some(n as u32);
        }
        if let Some(n) = load_env_u64("MSS") {
            cfg.force_mss = Some(n as u32);
        }
        if let Some(n) = load_env_u64("REUSEADDR") {
            cfg.force_reuseaddr = Some(n as u32);
        }
        if let Some(n) = load_env_u64("NODELAY") {
            cfg.force_nodelay = Some(n as u32);
        }
        if let Some(n) = load_env_u64("FLOWINFO") {
            cfg.force_flowinfo = Some((n as u32) & crate::shim::consts::IPV6_FLOWINFO_MASK);
        }
        if let Some(n) = load_env_u64("FWMARK") {
            cfg.force_fwmark = Some(n as u32);
        }
        if let Some(n) = load_env_u64("PRIO") {
            cfg.force_prio = Some(n as u32);
        }
        if let Ok(s) = std::env::var("BIND_DEVICE") {
            cfg.force_bind_to_device = Some(s);
        }

        // ---- Bandwidth ----
        if let Some(n) = load_env_u64("BW") {
            cfg.bw_limit_global = n;
        }
        if let Some(n) = load_env_u64("BW_PER_SOCKET") {
            if cfg.bw_limit_global > 0 {
                // The C version logs a warning here; we drop the log because
                // it would re-trigger the init-time logging deadlock.
            } else {
                cfg.bw_limit_per_socket = n;
            }
        }

        // ---- poll(2) timeout ----
        if let Some(n) = load_env_i64("POLL_TIMEOUT")
            && let Ok(t) = i32::try_from(n)
        {
            cfg.force_poll_timeout = Some(t);
        }

        cfg
    }
}

// =====================================================================
// env var helpers
// =====================================================================

/// Read `name` from the environment and parse as `u64` with `strtoul`-style
/// base detection: `0x` / `0X` prefix → hex, leading `0` (and len > 1) → octal,
/// else decimal. Returns `None` if the env var is unset or unparseable.
fn load_env_u64(name: &str) -> Option<u64> {
    let s = std::env::var(name).ok()?;
    parse_int_base0(&s)
}

fn load_env_i64(name: &str) -> Option<i64> {
    let s = std::env::var(name).ok()?;
    // Match the C version's `strtoul → (int)` cast: take the low 64 bits
    // and reinterpret as signed (i.e. wrap-around on overflow).
    Some(parse_int_base0(&s)?.cast_signed())
}

/// Parse an integer literal with the same base-detection rules as
/// `strtoul(s, NULL, 0)`: `0x`/`0X` → hex; leading `0` with len > 1 → octal;
/// otherwise decimal. Negative numbers are handled by an optional leading `-`.
pub(crate) fn parse_int_base0(s: &str) -> Option<u64> {
    let s = s.trim();
    let (neg, body) = if let Some(rest) = s.strip_prefix('-') {
        (true, rest)
    } else {
        (false, s)
    };
    let n: u64 = if let Some(hex) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16).ok()?
    } else if body.len() > 1 && body.starts_with('0') {
        u64::from_str_radix(&body[1..], 8).ok()?
    } else {
        body.parse::<u64>().ok()?
    };
    Some(if neg {
        (n.cast_signed()).wrapping_neg() as u64
    } else {
        n
    })
}

#[cfg(test)]
mod tests {
    use super::parse_int_base0;

    #[test]
    fn decimal() {
        assert_eq!(parse_int_base0("123"), Some(123));
    }

    #[test]
    fn hex() {
        assert_eq!(parse_int_base0("0xff"), Some(0xff));
        assert_eq!(parse_int_base0("0XFF"), Some(0xff));
    }

    #[test]
    fn octal() {
        assert_eq!(parse_int_base0("0755"), Some(0o755));
    }

    #[test]
    fn whitespace() {
        assert_eq!(parse_int_base0("  42  "), Some(42));
    }

    #[test]
    fn invalid() {
        assert_eq!(parse_int_base0(""), None);
        assert_eq!(parse_int_base0("xyz"), None);
    }
}
