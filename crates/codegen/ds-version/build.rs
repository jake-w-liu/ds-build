//! Bake product version + git short SHA into this crate only.
//!
//! All CLI surfaces must read version strings from `ds_version` so a partial
//! bump of some other crate's Cargo.toml cannot desync `ds --version`.

use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/heads");
    println!("cargo:rerun-if-env-changed=DS_VERSION");
    println!("cargo:rerun-if-env-changed=DS_GIT_COMMIT");

    let commit = std::env::var("DS_GIT_COMMIT").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    });

    let version = std::env::var("DS_VERSION")
        .or_else(|_| std::env::var("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| "0.0.0".to_string());

    println!("cargo:rustc-env=DS_GIT_COMMIT={commit}");
    // Full string used by every CLI path — single compile-time source.
    println!("cargo:rustc-env=DS_VERSION_WITH_COMMIT={version} ({commit})");
}
