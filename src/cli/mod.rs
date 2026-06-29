//! CLI subcommands (`run`, `show`, `profile list`).

#![allow(dead_code)]

use clap::{Parser, Subcommand};
use std::collections::HashMap;

pub mod config;
pub mod flags;
pub mod generate;
pub mod launch;
pub mod profile;

#[derive(Parser)]
#[command(
    name = "rbind",
    about = "Run a command under rbind force-routing",
    version
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Run a command with rbind force-routing applied.
    Run {
        /// Optional profile name from `~/.config/rbind/profiles.toml`.
        #[arg(long, short = 'p')]
        profile: Option<String>,

        #[command(flatten)]
        flags: flags::ForceFlags,

        /// Command and args after `--`.
        #[arg(last = true, required = true)]
        cmd: Vec<String>,
    },

    /// Show the env-var set a profile+flags combo would produce (no execution).
    Show {
        #[arg(long, short = 'p')]
        profile: Option<String>,
        #[command(flatten)]
        flags: flags::ForceFlags,
    },

    /// Profile management.
    Profile {
        #[command(subcommand)]
        cmd: ProfileCmd,
    },

    /// Build the shim's `.so` and place it next to the running binary.
    Generate,
}

#[derive(Subcommand)]
pub enum ProfileCmd {
    /// List profiles found in `~/.config/rbind/profiles.toml`.
    List {
        /// Show full env-var set for this profile (instead of just names).
        #[arg(long)]
        name: Option<String>,
    },
}

pub fn run() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Run {
            profile,
            flags,
            cmd,
        } => {
            let env = build_env(profile.as_deref(), &flags)?;
            let so = launch::find_so()?;
            launch::exec_with_env(&so, &env, &cmd)?;
        }
        Command::Show { profile, flags } => {
            let env = build_env(profile.as_deref(), &flags)?;
            let so = launch::find_so()?;
            println!("LD_PRELOAD={}", so.display());
            for (k, v) in &env {
                println!("{k}={v}");
            }
        }
        Command::Profile {
            cmd: ProfileCmd::List { name },
        } => {
            let profiles = profile::load_profiles().unwrap_or_default();
            match name {
                None => {
                    let names = profile::sorted_names(&profiles);
                    if names.is_empty() {
                        println!("(no profiles loaded)");
                    } else {
                        for n in names {
                            println!("{n}");
                        }
                    }
                }
                Some(n) => {
                    let env = profile::profile_to_env(&profiles, &n)?;
                    for (k, v) in &env {
                        println!("{k}={v}");
                    }
                }
            }
        }
        Command::Generate => {
            generate::run()?;
        }
    }
    Ok(())
}

fn build_env(
    profile: Option<&str>,
    flags: &flags::ForceFlags,
) -> anyhow::Result<HashMap<String, String>> {
    let mut env: HashMap<String, String> = HashMap::new();
    if let Some(name) = profile {
        let profiles = profile::load_profiles()?;
        env.extend(profile::profile_to_env(&profiles, name)?);
    }
    flags.apply_to(&mut env); // flags always win over profile values
    Ok(env)
}
