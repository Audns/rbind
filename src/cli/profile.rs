//! TOML profile loader + env-var projection.

#![allow(dead_code)]

use serde::Deserialize;
use std::collections::HashMap;
use std::path::PathBuf;

/// In-memory representation of `~/.config/rbind/profiles.toml`. Each table is
/// a named profile; the keys inside are the raw env-var names the shim
/// reads (`ADDRESS_V4`, `BW`, `BIND_DEVICE`, …).
#[derive(Deserialize, Debug, Default)]
pub struct Profiles {
    #[serde(flatten)]
    pub entries: HashMap<String, HashMap<String, toml::Value>>,
}

/// Path to the user's profile file. Returns `None` only if the OS has no
/// config dir at all (vanishingly rare on Linux).
#[must_use]
pub fn profile_path() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("rbind").join("profiles.toml"))
}

/// Read the profile file. Missing file is *not* an error — that's the
/// default state on a fresh install, and the CLI should treat it as
/// "no profiles defined".
pub fn load_profiles() -> anyhow::Result<Profiles> {
    let Some(path) = profile_path() else {
        return Ok(Profiles::default());
    };
    if !path.exists() {
        return Ok(Profiles::default());
    }
    let text = std::fs::read_to_string(&path)
        .map_err(|e| anyhow::anyhow!("reading {}: {e}", path.display()))?;
    let profiles: Profiles =
        toml::from_str(&text).map_err(|e| anyhow::anyhow!("parsing {}: {e}", path.display()))?;
    Ok(profiles)
}

/// Project a named profile's table into a flat `String → String` env map.
/// Returns an error if the named profile doesn't exist.
pub fn profile_to_env(profiles: &Profiles, name: &str) -> anyhow::Result<HashMap<String, String>> {
    let entry = profiles
        .entries
        .get(name)
        .ok_or_else(|| anyhow::anyhow!("no profile named '{name}' in profiles.toml"))?;
    let mut env = HashMap::new();
    for (k, v) in entry {
        env.insert(k.clone(), toml_value_to_string(v));
    }
    Ok(env)
}

/// Sorted list of profile names — used by `rbind profile list`.
#[must_use]
pub fn sorted_names(p: &Profiles) -> Vec<String> {
    let mut n: Vec<String> = p.entries.keys().cloned().collect();
    n.sort();
    n
}

fn toml_value_to_string(v: &toml::Value) -> String {
    match v {
        toml::Value::String(s) => s.clone(),
        toml::Value::Integer(i) => i.to_string(),
        toml::Value::Float(f) => f.to_string(),
        toml::Value::Boolean(b) => {
            if *b {
                "1".into()
            } else {
                "0".into()
            }
        }
        other => other.to_string(),
    }
}
