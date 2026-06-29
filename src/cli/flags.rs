//! CLI flag → env-var mapping.

#![allow(dead_code)]

use clap::Args;
use std::collections::HashMap;

/// All `--flag` options that map directly to a short env var matching the
/// flag name (e.g. `--bind-device` → `BIND_DEVICE`).
///
/// Flags are merged on top of profile values — flag wins.
#[derive(Args, Debug, Default)]
pub struct ForceFlags {
    // --- bind address / port ---
    #[arg(
        long,
        help = "Forced IPv4 source address (e.g. 10.0.0.5, 'deny', 'fake')"
    )]
    pub address_v4: Option<String>,
    #[arg(long, help = "Forced IPv6 source address")]
    pub address_v6: Option<String>,
    #[arg(long, help = "Forced IPv4 port (host order)")]
    pub port_v4: Option<u16>,
    #[arg(long, help = "Forced IPv6 port (host order)")]
    pub port_v6: Option<u16>,

    // --- forced socket options ---
    #[arg(
        long,
        help = "Bind every socket to this interface (e.g. wg0). Needs CAP_NET_RAW"
    )]
    pub bind_device: Option<String>,
    #[arg(long, help = "IP TOS / DSCP field (e.g. 0xb8)")]
    pub tos: Option<String>,
    #[arg(long, help = "IPv4 TTL")]
    pub ttl: Option<u8>,
    #[arg(long, help = "TCP keepalive idle time in seconds (0 disables)")]
    pub ka: Option<u32>,
    #[arg(long, help = "TCP max segment size")]
    pub mss: Option<u32>,
    #[arg(long, help = "Netfilter packet mark (root required)")]
    pub fwmark: Option<String>,
    #[arg(long, help = "prio qdisc band (0–6)")]
    pub prio: Option<u32>,

    // --- TCP-only toggles (booleans) ---
    #[arg(long, help = "Force SO_REUSEADDR=1")]
    pub reuseaddr: bool,
    #[arg(long, help = "Force TCP_NODELAY=1")]
    pub nodelay: bool,

    // --- bandwidth (accepted as "1mb", "65536", etc.) ---
    #[arg(
        long,
        value_name = "RATE",
        help = "Cap all sockets to N bytes/sec (e.g. 65536, 1kb, 1mb)"
    )]
    pub bw: Option<String>,
    #[arg(
        long,
        value_name = "RATE",
        help = "Cap each socket independently (mutually exclusive with --bw)"
    )]
    pub bw_per_socket: Option<String>,

    // --- misc ---
    #[arg(long, help = "Override every poll(2) timeout (ms)")]
    pub poll_timeout: Option<i32>,

    // --- debug ---
    #[arg(long, help = "Verbosity: 0=errors, 1=every hook, 2=+throttle timing")]
    pub verbose: Option<u32>,
    #[arg(
        long,
        help = "Write a per-line trace of every intercepted syscall to this path"
    )]
    pub log: Option<String>,
}

impl ForceFlags {
    /// Write the flag values into `env`, skipping any flag that wasn't set.
    /// `env` is mutated in place; previously-set keys are overwritten.
    pub fn apply_to(&self, env: &mut HashMap<String, String>) {
        set_opt(env, "ADDRESS_V4", self.address_v4.as_ref());
        set_opt(env, "ADDRESS_V6", self.address_v6.as_ref());
        set_opt_u16(env, "PORT_V4", self.port_v4);
        set_opt_u16(env, "PORT_V6", self.port_v6);

        set_opt(env, "BIND_DEVICE", self.bind_device.as_ref());
        set_opt(env, "TOS", self.tos.as_ref());
        set_opt_u8(env, "TTL", self.ttl);
        set_opt_u32(env, "KA", self.ka);
        set_opt_u32(env, "MSS", self.mss);
        set_opt(env, "FWMARK", self.fwmark.as_ref());
        set_opt_u32(env, "PRIO", self.prio);

        if self.reuseaddr {
            env.insert("REUSEADDR".into(), "1".into());
        }
        if self.nodelay {
            env.insert("NODELAY".into(), "1".into());
        }

        if let Some(ref bw) = self.bw {
            env.insert("BW".into(), parse_bandwidth(bw).to_string());
        }
        if let Some(ref bw) = self.bw_per_socket {
            env.insert("BW_PER_SOCKET".into(), parse_bandwidth(bw).to_string());
        }

        set_opt_i32(env, "POLL_TIMEOUT", self.poll_timeout);

        set_opt_u32(env, "VERBOSE", self.verbose);
        set_opt(env, "LOG", self.log.as_ref());
    }
}

fn set_opt(env: &mut HashMap<String, String>, key: &str, val: Option<&String>) {
    if let Some(v) = val {
        env.insert(key.to_string(), v.clone());
    }
}

fn set_opt_u8(env: &mut HashMap<String, String>, key: &str, val: Option<u8>) {
    if let Some(v) = val {
        env.insert(key.to_string(), v.to_string());
    }
}

fn set_opt_u16(env: &mut HashMap<String, String>, key: &str, val: Option<u16>) {
    if let Some(v) = val {
        env.insert(key.to_string(), v.to_string());
    }
}

fn set_opt_u32(env: &mut HashMap<String, String>, key: &str, val: Option<u32>) {
    if let Some(v) = val {
        env.insert(key.to_string(), v.to_string());
    }
}

fn set_opt_i32(env: &mut HashMap<String, String>, key: &str, val: Option<i32>) {
    if let Some(v) = val {
        env.insert(key.to_string(), v.to_string());
    }
}

/// Accept bandwidth strings like `"65536"`, `"1kb"`/`"1024kb"`, `"1mb"`/`"1MB"`,
/// case-insensitive. Returns `0` on parse failure (which the env var loader will
/// treat as "no limit", so the field is harmless if e.g. `--bw banana` is passed).
#[must_use]
pub fn parse_bandwidth(s: &str) -> u64 {
    let s = s.trim().to_ascii_lowercase();
    let (num_part, mult): (&str, u64) = if let Some(n) = s.strip_suffix("mb") {
        (n, 1_048_576)
    } else if let Some(n) = s.strip_suffix("kb") {
        (n, 1_024)
    } else if let Some(n) = s.strip_suffix('b') {
        (n, 1)
    } else {
        (s.as_str(), 1)
    };
    num_part
        .trim()
        .parse::<u64>()
        .unwrap_or(0)
        .saturating_mul(mult)
}

#[cfg(test)]
mod tests {
    use super::parse_bandwidth;

    #[test]
    fn parse_bandwidth_plain_bytes() {
        assert_eq!(parse_bandwidth("65536"), 65536);
        assert_eq!(parse_bandwidth("0"), 0);
    }

    #[test]
    fn parse_bandwidth_suffixes() {
        assert_eq!(parse_bandwidth("1kb"), 1_024);
        assert_eq!(parse_bandwidth("512KB"), 512 * 1_024);
        assert_eq!(parse_bandwidth("1mb"), 1_048_576);
        assert_eq!(parse_bandwidth("2MB"), 2 * 1_048_576);
    }

    #[test]
    fn parse_bandwidth_garbage_is_zero() {
        assert_eq!(parse_bandwidth("banana"), 0);
        assert_eq!(parse_bandwidth(""), 0);
    }
}
