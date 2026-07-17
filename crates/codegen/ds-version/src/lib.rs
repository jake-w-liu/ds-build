//! Installed ds CLI version, lockstepped with shipping binaries.
//!
//! # Single source of truth
//!
//! | Piece | Source |
//! |-------|--------|
//! | Semver (`0.1.2`) | This crate's `Cargo.toml` / `DS_VERSION` |
//! | Git short SHA | This crate's `build.rs` |
//! | Full `"0.1.2 (abc)"` | [`VERSION_WITH_COMMIT`] |
//!
//! All user-facing version strings (`ds --version`, `ds version --json`,
//! startup banners, Sentry release, OTEL) must go through this crate so a
//! partial Cargo.toml bump cannot leave the CLI stuck on an older number.

use semver::Version;

pub const TEST_VERSION_ENV: &str = "DS_TEST_VERSION";

/// Product semver. Prefer over any other crate's `CARGO_PKG_VERSION`.
pub const VERSION: &str = match option_env!("DS_VERSION") {
    Some(v) => v,
    None => env!("CARGO_PKG_VERSION"),
};

/// Short git commit baked by this crate's `build.rs`.
pub const GIT_COMMIT: &str = match option_env!("DS_GIT_COMMIT") {
    Some(c) if !c.is_empty() => c,
    _ => "unknown",
};

/// Full compile-time string: `"0.1.2 (abc1234)"`.
///
/// Prefer this (or [`display_cli_version`]) everywhere a version string is
/// shown or reported. Do **not** re-derive from another crate's package version.
pub const VERSION_WITH_COMMIT: &str = match option_env!("DS_VERSION_WITH_COMMIT") {
    Some(v) => v,
    // Fallbacks for unit tests / docs that compile without build.rs env.
    None => concat!(env!("CARGO_PKG_VERSION"), " (unknown)"),
};

/// Owned copy of [`VERSION_WITH_COMMIT`] (for APIs that want `String`).
pub fn version_with_commit() -> String {
    VERSION_WITH_COMMIT.to_string()
}

/// [`TEST_VERSION_ENV`] override first, then [`VERSION`]. Trimmed so
/// non-semver-aware callers can pass the result straight into parsing.
pub fn installed() -> String {
    std::env::var(TEST_VERSION_ENV)
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|_| VERSION.to_string())
}

pub fn installed_semver() -> Result<Version, semver::Error> {
    Version::parse(&installed())
}

/// Format the compiled version with a channel label for user-facing display.
///
/// `channel_label` is a pre-formatted suffix such as `" [alpha]"`, `" [stable]"`,
/// or `""` (empty when no cached pointer is available). Obtain it from
/// `ds_update::channel_label()`.
///
/// Example: `"0.2.5 [stable]"` or `"0.2.5 [alpha]"`.
pub fn display_version(channel_label: &str) -> String {
    format!("{VERSION}{channel_label}")
}

/// Format a version-with-commit string with a channel label.
///
/// Prefer [`display_cli_version`] for CLI output so semver always comes
/// from this crate. This helper remains for callers that already hold a
/// full `"0.2.5 (abc1234)"` string (e.g. tests).
///
/// Example: `"0.2.5 (abc1234) [stable]"`.
pub fn display_version_with_commit(version_with_commit: &str, channel_label: &str) -> String {
    format!("{version_with_commit}{channel_label}")
}

/// Canonical CLI version line body: [`VERSION_WITH_COMMIT`] plus optional channel.
///
/// Use this for `ds --version`, `ds version`, and startup banners.
pub fn display_cli_version(channel_label: &str) -> String {
    display_version_with_commit(VERSION_WITH_COMMIT, channel_label)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Display formatting invariant matrix — verifies label appending
    /// works correctly across all label states (alpha, stable, empty).
    #[test]
    fn test_display_version_formatting_matrix() {
        let cases: &[(&str, &str, &str)] = &[
            // (version_with_commit,    label,        expected_suffix)
            ("0.2.5 (abc1234)", " [alpha]", "0.2.5 (abc1234) [alpha]"),
            ("0.2.5 (abc1234)", " [stable]", "0.2.5 (abc1234) [stable]"),
            ("0.2.5 (abc1234)", "", "0.2.5 (abc1234)"),
            (
                "0.1.220-alpha.2 (def0)",
                " [alpha]",
                "0.1.220-alpha.2 (def0) [alpha]",
            ),
        ];
        for (vwc, label, expected) in cases {
            assert_eq!(
                display_version_with_commit(vwc, label),
                *expected,
                "display_version_with_commit({:?}, {:?})",
                vwc,
                label,
            );
        }
        // display_version uses compiled VERSION — just verify the label appends
        assert_eq!(display_version(""), VERSION);
        assert!(display_version(" [stable]").ends_with("[stable]"));
    }

    #[test]
    fn version_with_commit_locksteps_crate_version() {
        assert!(
            VERSION_WITH_COMMIT.starts_with(VERSION),
            "VERSION_WITH_COMMIT must start with VERSION={VERSION:?}, got {VERSION_WITH_COMMIT:?}"
        );
        assert!(
            VERSION_WITH_COMMIT.contains('(') && VERSION_WITH_COMMIT.ends_with(')'),
            "expected 'semver (commit)' form, got {VERSION_WITH_COMMIT:?}"
        );
        assert_eq!(version_with_commit(), VERSION_WITH_COMMIT);
    }

    #[test]
    fn display_cli_version_is_lockstepped() {
        let s = display_cli_version("");
        assert_eq!(s, VERSION_WITH_COMMIT);
        assert!(s.starts_with(VERSION));
        let labeled = display_cli_version(" [stable]");
        assert!(labeled.starts_with(VERSION));
        assert!(labeled.ends_with(" [stable]"));
    }

    #[test]
    fn git_commit_is_nonempty() {
        assert!(!GIT_COMMIT.is_empty());
    }
}
