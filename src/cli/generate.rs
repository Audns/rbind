//! `rbind generate` — build the shim's `.so` and place it next to the
//! running `rbind` binary so subsequent invocations find it via the
//! directory-walk step in [`super::launch::find_so`].
//!
//! This is a thin wrapper: it runs `cargo build --release` from the project
//! root (assumed to be the current working directory), then copies
//! `target/release/librbind_lib.so` into the directory containing the
//! `rbind` binary itself.

#![allow(dead_code)]

use std::path::PathBuf;
use std::process::Command;

/// Build the cdylib in release mode and copy it next to the running
/// `rbind` binary. Fails if `Cargo.toml` is not in the current directory,
/// if `cargo build` exits non-zero, or if the expected artifact is missing
/// from `target/release/`.
pub fn run() -> anyhow::Result<()> {
    if !PathBuf::from("Cargo.toml").exists() {
        anyhow::bail!(
            "Cargo.toml not found in current directory — run from the rbind project root"
        );
    }

    let status = Command::new("cargo")
        .arg("build")
        .arg("--release")
        .status()
        .map_err(|e| anyhow::anyhow!("failed to spawn cargo: {e}"))?;
    if !status.success() {
        anyhow::bail!("cargo build failed (exit status: {status})");
    }

    let artifact = PathBuf::from("target")
        .join("release")
        .join("librbind_lib.so");
    if !artifact.exists() {
        anyhow::bail!(
            "expected artifact not found at {} — did the cdylib build?",
            artifact.display()
        );
    }

    let exe = std::env::current_exe().map_err(|e| anyhow::anyhow!("locating current exe: {e}"))?;
    let dest_dir = exe
        .parent()
        .ok_or_else(|| anyhow::anyhow!("current exe has no parent directory"))?;
    let dest = dest_dir.join("librbind_lib.so");

    std::fs::copy(&artifact, &dest).map_err(|e| {
        anyhow::anyhow!("copying {} -> {}: {e}", artifact.display(), dest.display())
    })?;
    println!("wrote {}", dest.display());
    Ok(())
}
