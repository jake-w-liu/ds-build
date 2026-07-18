//! Built-in files extracted to `~/.ds/` on startup.

const BUNDLED_FILES: &[(&str, &str)] = &[("README.md", include_str!("../README.md"))];

const HELP_SKILL_MD: &str = include_str!("../skills/help/SKILL.md");
const CREATE_SKILL_MD: &str = include_str!("../skills/create-skill/SKILL.md");
const CODE_REVIEW_SKILL_MD: &str = include_str!("../skills/code-review/SKILL.md");
const IMAGINE_SKILL_MD: &str = include_str!("../skills/imagine/SKILL.md");
/// Compiled-in SKILL.md content for `/check-work` (available to headless mode).
pub const CHECK_SKILL_MD: &str = include_str!("../skills/check-work/SKILL.md");
/// Compiled-in SKILL.md content for headless `--best-of-n` (not extracted as
/// a bundled skill).
pub const BEST_OF_N_SKILL_MD: &str = include_str!("../skills/best-of-n/SKILL.md");
const DEEP_DEBUG_SKILL_MD: &str = include_str!("../skills/deep-debug/SKILL.md");
const SILENCE_SKILL_MD: &str = include_str!("../skills/silence/SKILL.md");
const FABLE_SKILL_MD: &str = include_str!("../skills/fable/SKILL.md");
const FABLE_LOOP_SKILL_MD: &str = include_str!("../skills/fable-loop/SKILL.md");
const PSST_GPT_SKILL_MD: &str = include_str!("../skills/psst-gpt/SKILL.md");

/// Extra files shipped beside a skill's SKILL.md (e.g. scripts).
/// Path is relative to `skills/<name>/`.
const BUNDLED_SKILL_ASSETS: &[(&str, &str, &str)] = &[
    (
        "psst-gpt",
        "scripts/psst_chat_relay.swift",
        include_str!("../skills/psst-gpt/scripts/psst_chat_relay.swift"),
    ),
    (
        "psst-gpt",
        "scripts/psst_gpt.mjs",
        include_str!("../skills/psst-gpt/scripts/psst_gpt.mjs"),
    ),
    (
        "psst-gpt",
        "scripts/psst_ax_upload.swift",
        include_str!("../skills/psst-gpt/scripts/psst_ax_upload.swift"),
    ),
    (
        "psst-gpt",
        "scripts/psst_zip_upload.swift",
        include_str!("../skills/psst-gpt/scripts/psst_zip_upload.swift"),
    ),
    (
        "psst-gpt",
        "scripts/run_full_codebase_audit.sh",
        include_str!("../skills/psst-gpt/scripts/run_full_codebase_audit.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_absorb.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_absorb.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_finish_rules.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_finish_rules.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_generation_policy.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_generation_policy.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_longrun_policy.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_longrun_policy.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_relay_policy.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_relay_policy.sh"),
    ),
    (
        "psst-gpt",
        "scripts/selfcheck_wake.sh",
        include_str!("../skills/psst-gpt/scripts/selfcheck_wake.sh"),
    ),
];

/// Legacy bundled skill names (renamed or removed).
///
/// These directories under `~/.ds/skills/` will be deleted on startup
/// (during bundled file extraction). This ensures that when a bundled
/// skill is renamed (e.g. `check` → `check-work`), the old slash command
/// does not linger on users' machines after an upgrade.
///
/// Important behavior:
/// - Deletion happens **early** in `extract_bundled_files`, before we write
///   any current bundled skills.
/// - We **never** delete a name that is currently present in `BUNDLED_SKILLS`
///   (see `remove_legacy_bundled_skills`).
///
/// This means:
/// - If you later re-introduce a skill with a name that is still in this
///   legacy list (e.g. you ship a new "check" skill years later), the legacy
///   cleanup will **skip** it and the new skill will be created normally.
/// - The legacy list is a "delete old user copies of names we no longer ship",
///   not a permanent blacklist.
///
/// Lifecycle / maintenance:
/// - Add an old name here when you rename/remove a bundled skill.
/// - Once the directory is gone on a user's machine, further checks are
///   cheap no-ops.
/// - You do **not** have to remove entries immediately. It is safe to leave
///   them for many releases.
/// - After the rename has had time to propagate, you **may** clean old
///   strings out of this list for hygiene.
const LEGACY_BUNDLED_SKILL_NAMES: &[&str] = &["check", "best-of-n", "docx", "pptx", "xlsx"];

/// All bundled skill SKILL.md files. Single source of truth used by both
/// the full extraction path (version bump) and the missing-file fast path
/// (same version). Adding a new skill here is all that's needed.
///
/// When renaming a bundled skill (e.g. "check" → "check-work"), also add the
/// old name to `LEGACY_BUNDLED_SKILL_NAMES` so `remove_legacy_bundled_skills`
/// will clean up the old directory on user machines on the next upgrade.
///
/// See the docs on `LEGACY_BUNDLED_SKILL_NAMES` for the full lifecycle
/// (including when it is safe/optional to remove old entries later).
const BUNDLED_SKILLS: &[(&str, &str)] = &[
    ("help", HELP_SKILL_MD),
    ("create-skill", CREATE_SKILL_MD),
    ("code-review", CODE_REVIEW_SKILL_MD),
    ("imagine", IMAGINE_SKILL_MD),
    ("check-work", CHECK_SKILL_MD),
    ("deep-debug", DEEP_DEBUG_SKILL_MD),
    ("silence", SILENCE_SKILL_MD),
    ("fable", FABLE_SKILL_MD),
    ("fable-loop", FABLE_LOOP_SKILL_MD),
    ("psst-gpt", PSST_GPT_SKILL_MD),
];

/// True when a discovered skill is the copy `extract_bundled_files` wrote to
/// `<ds_home>/skills/<name>/SKILL.md`. Exact-path (not prefix) so a
/// user-authored skill that reuses a bundled name — even elsewhere under
/// `<ds_home>/skills/` — is never labeled bundled. Lives beside the
/// extraction code so the target layout and this predicate move together.
/// Used by inspect, which otherwise sees extracted copies as user skills.
pub(crate) fn is_extracted_bundled_skill(
    name: &str,
    path: &std::path::Path,
    ds_home: &std::path::Path,
) -> bool {
    BUNDLED_SKILLS.iter().any(|&(n, _)| n == name)
        && path == ds_home.join("skills").join(name).join("SKILL.md")
}

/// Resolve the content for a skill, applying any name-specific transforms.
fn resolve_skill_content(name: &str, raw: &str, ds_home: &std::path::Path) -> String {
    match name {
        // Help skill needs path substitution so absolute paths work.
        "help" => {
            let ds_home_str = format!("{}/", ds_home.to_string_lossy());
            raw.replace("~/.ds/", &ds_home_str)
        }
        _ => raw.to_string(),
    }
}

/// Extract bundled files to `~/.ds/` on startup.
///
/// Full extraction runs on every version bump. On same-version startups,
/// a lightweight check ensures all expected skill files exist on disk —
/// any missing files are extracted individually.
///
/// Legacy/renamed bundled skills (see `LEGACY_BUNDLED_SKILL_NAMES`) are
/// always cleaned up first so that old slash commands disappear after
/// a rename (e.g. the previous `/check` after the move to `/check-work`).
pub fn extract_bundled_files(ds_home: &std::path::Path) {
    // Always remove legacy/renamed bundled skills first (e.g. the old
    // `check` directory after the rename to `check-work`). This runs on
    // every startup so users get cleaned up even without hitting a
    // version-bump marker change.
    remove_legacy_bundled_skills(ds_home);

    let version = ds_version::VERSION;
    let marker = ds_home.join(".metadata_version");

    if let Ok(existing) = std::fs::read_to_string(&marker)
        && existing.trim() == version
    {
        // Same version — only extract skill files that are missing on disk.
        // This handles skills added between version bumps.
        extract_missing_skills(ds_home);
        return;
    }

    let _ = std::fs::create_dir_all(ds_home);

    // Clean up cached changelog files from previous version so
    // /release-notes fetches fresh content for the new version.
    for stale in &["CHANGELOG.json", "CHANGELOG.md"] {
        let _ = std::fs::remove_file(ds_home.join(stale));
    }

    for &(filename, content) in BUNDLED_FILES {
        if let Err(e) = std::fs::write(ds_home.join(filename), content) {
            tracing::debug!(error = %e, filename, "Failed to extract bundled file");
        }
    }

    // Skill SKILL.md files.
    for &(name, raw) in BUNDLED_SKILLS {
        let skill_dir = ds_home.join("skills").join(name);
        let _ = std::fs::create_dir_all(&skill_dir);
        let content = resolve_skill_content(name, raw, ds_home);
        if let Err(e) = std::fs::write(skill_dir.join("SKILL.md"), content) {
            tracing::debug!(error = %e, name, "Failed to write skill");
        }
    }
    extract_skill_assets(ds_home);

    let _ = std::fs::write(&marker, version);
    tracing::debug!(version, "Extracted bundled files");
}

/// Extract or refresh bundled skill SKILL.md files (same-version fast path).
///
/// Iterates `BUNDLED_SKILLS` so adding a new skill there is sufficient.
/// Also rewrites when on-disk content differs from the compiled-in body so
/// skill edits ship without requiring a version-marker bump (avoids stale
/// prompts after upgrades that only change skill text).
fn extract_missing_skills(ds_home: &std::path::Path) {
    for &(name, raw) in BUNDLED_SKILLS {
        let skill_md = ds_home.join("skills").join(name).join("SKILL.md");
        let content = resolve_skill_content(name, raw, ds_home);
        if skill_md.exists()
            && let Ok(existing) = std::fs::read_to_string(&skill_md)
            && existing == content
        {
            continue;
        }
        let _ = std::fs::create_dir_all(skill_md.parent().unwrap());
        if let Err(e) = std::fs::write(&skill_md, &content) {
            tracing::debug!(error = %e, name, "Failed to write/refresh skill");
        }
    }
    extract_skill_assets(ds_home);
}

/// Write skill-adjacent assets (scripts, helpers) under `skills/<name>/…`.
/// Refreshes when on-disk content differs so script fixes ship without a
/// full version-marker bump.
fn extract_skill_assets(ds_home: &std::path::Path) {
    for &(skill, rel, content) in BUNDLED_SKILL_ASSETS {
        let path = ds_home.join("skills").join(skill).join(rel);
        if path.exists()
            && let Ok(existing) = std::fs::read_to_string(&path)
            && existing == content
        {
            continue;
        }
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Err(e) = std::fs::write(&path, content) {
            tracing::debug!(error = %e, skill, rel, "Failed to write skill asset");
        }
    }
}

/// Remove directories for legacy/renamed bundled skills (e.g. old `check`
/// after it was renamed to `check-work`).
///
/// Called on every startup from `extract_bundled_files`. Safe and idempotent.
///
/// Key guarantees (see `LEGACY_BUNDLED_SKILL_NAMES` docs for details):
/// - If a name is still present in `BUNDLED_SKILLS`, we deliberately skip
///   deletion. This allows safe re-use of a skill name in the future.
/// - If the target directory no longer exists, this is a trivial no-op.
fn remove_legacy_bundled_skills(ds_home: &std::path::Path) {
    remove_legacy_skills(ds_home, LEGACY_BUNDLED_SKILL_NAMES, BUNDLED_SKILLS);
}

/// Core implementation, extracted for testability.
fn remove_legacy_skills(
    ds_home: &std::path::Path,
    legacy_names: &[&str],
    bundled_skills: &[(&str, &str)],
) {
    for name in legacy_names {
        // Safety: Never delete a name that we are currently shipping.
        // This protects against re-introducing a skill name that still has
        // an entry in the legacy list.
        if bundled_skills.iter().any(|(n, _)| *n == *name) {
            continue;
        }

        let dir = ds_home.join("skills").join(name);
        if dir.exists() {
            if let Err(e) = std::fs::remove_dir_all(&dir) {
                tracing::debug!(error = %e, name, "Failed to remove legacy bundled skill");
            } else {
                tracing::debug!(name, "Removed legacy bundled skill directory");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_bump_re_extracts_all_files() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        extract_bundled_files(home);

        for &(filename, _) in BUNDLED_FILES {
            std::fs::write(home.join(filename), "old").unwrap();
        }
        std::fs::write(home.join("skills/help/SKILL.md"), "old").unwrap();
        for name in ["check-work", "imagine", "code-review"] {
            std::fs::write(home.join(format!("skills/{name}/SKILL.md")), "old").unwrap();
        }
        std::fs::write(home.join(".metadata_version"), "0.0.0-stale").unwrap();

        // Simulate legacy skills that should be cleaned up.
        for name in ["check", "best-of-n", "docx", "pptx", "xlsx"] {
            std::fs::create_dir_all(home.join(format!("skills/{name}"))).unwrap();
            std::fs::write(
                home.join(format!("skills/{name}/SKILL.md")),
                "old legacy skill",
            )
            .unwrap();
        }

        extract_bundled_files(home);

        for &(filename, _) in BUNDLED_FILES {
            assert_ne!(
                std::fs::read_to_string(home.join(filename)).unwrap(),
                "old",
                "{filename} was not re-extracted after version bump"
            );
        }
        assert_ne!(
            std::fs::read_to_string(home.join("skills/help/SKILL.md")).unwrap(),
            "old"
        );
        for name in ["check-work", "imagine", "code-review"] {
            assert_ne!(
                std::fs::read_to_string(home.join(format!("skills/{name}/SKILL.md"))).unwrap(),
                "old",
                "{name} skill was not re-extracted after version bump"
            );
        }

        // Legacy skill directories must have been removed (the key part of
        // supporting renames like check → check-work without leaving orphans).
        for name in ["check", "best-of-n", "docx", "pptx", "xlsx"] {
            assert!(
                !home.join(format!("skills/{name}")).exists(),
                "legacy '{name}' skill directory should have been deleted during version bump"
            );
        }
    }

    #[test]
    fn office_skills_not_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        extract_bundled_files(home);

        // Former office document skills must NOT be extracted as bundled.
        for name in ["docx", "pptx", "xlsx"] {
            assert!(
                !home.join(format!("skills/{name}")).exists(),
                "{name} should not be a bundled skill"
            );
        }
    }

    #[tokio::test]
    async fn help_skill_discovered_by_skill_pipeline() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        extract_bundled_files(home);

        let workspace = tmp.path().join("workspace");
        std::fs::create_dir_all(workspace.join(".ds").join("skills").join("help")).unwrap();
        std::fs::copy(
            home.join("skills/help/SKILL.md"),
            workspace.join(".ds/skills/help/SKILL.md"),
        )
        .unwrap();

        let skills = ds_agent::prompt::skills::list_skills(
            Some(workspace.to_str().unwrap()),
            &Default::default(),
            ds_agent::prompt::skills::CompatConfig::default(),
        )
        .await;

        let help = skills.iter().find(|s| s.name == "help");
        assert!(
            help.is_some(),
            "help skill not found. skills: {:?}",
            skills.iter().map(|s| &s.name).collect::<Vec<_>>()
        );
        let help = help.unwrap();
        assert!(help.description.contains("configuration"));
        assert!(help.user_invocable);
    }

    // ---------------------------------------------------------------------
    // Tests for legacy bundled skill removal (the rename migration system)
    // ---------------------------------------------------------------------

    #[test]
    fn remove_legacy_deletes_old_skill_when_not_currently_shipped() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // Simulate an old legacy "check" directory from before a rename.
        let legacy_dir = home.join("skills/check");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("SKILL.md"), "old check").unwrap();

        // "check" is in legacy list but NOT in current BUNDLED_SKILLS
        remove_legacy_skills(home, &["check"], BUNDLED_SKILLS);

        assert!(
            !legacy_dir.exists(),
            "legacy skill directory should have been deleted"
        );
    }

    #[test]
    fn remove_legacy_does_not_delete_when_name_is_reused_in_current_bundled() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // User still has an old "check" directory.
        let legacy_dir = home.join("skills/check");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("SKILL.md"), "user had old check").unwrap();

        // Simulate the situation where we later re-ship a skill named "check".
        // In this case the legacy entry should be ignored.
        let fake_bundled: &[(&str, &str)] = &[("check", "fake content"), ("help", "help")];

        remove_legacy_skills(home, &["check"], fake_bundled);

        // The directory must still exist (we did not nuke the user's copy
        // or a skill we're about to (re)create).
        assert!(
            legacy_dir.exists(),
            "should not delete a name that is currently being shipped"
        );
    }

    #[test]
    fn remove_legacy_handles_multiple_names_some_current_some_legacy() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        std::fs::create_dir_all(home.join("skills/old-renamed")).unwrap();
        std::fs::write(home.join("skills/old-renamed/SKILL.md"), "old").unwrap();

        std::fs::create_dir_all(home.join("skills/another-legacy")).unwrap();
        std::fs::write(home.join("skills/another-legacy/SKILL.md"), "old2").unwrap();

        // Current bundled skills include one name that used to be legacy
        let current: &[(&str, &str)] = &[("another-legacy", "now shipping again")];

        // Legacy list contains both the truly removed one and the reintroduced one
        remove_legacy_skills(home, &["old-renamed", "another-legacy"], current);

        assert!(
            !home.join("skills/old-renamed").exists(),
            "truly legacy name should be removed"
        );
        assert!(
            home.join("skills/another-legacy").exists(),
            "reintroduced name must not be deleted"
        );
    }

    #[test]
    fn extract_missing_refreshes_stale_skill_content() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();
        extract_bundled_files(home);
        let path = home.join("skills/fable-loop/SKILL.md");
        assert!(path.exists(), "fable-loop should be extracted");
        std::fs::write(&path, "STALE_CONTENT").unwrap();
        // Same version marker — refresh path only.
        extract_missing_skills(home);
        let body = std::fs::read_to_string(&path).unwrap();
        assert!(
            body.contains("Fable Loop") || body.contains("orchestration"),
            "stale skill body must be refreshed from bundled content"
        );
        assert!(!body.contains("STALE_CONTENT"));
    }

    #[test]
    fn psst_gpt_bundles_every_shipped_script() {
        let expected: std::collections::BTreeSet<_> = [
            "scripts/psst_ax_upload.swift",
            "scripts/psst_chat_relay.swift",
            "scripts/psst_gpt.mjs",
            "scripts/psst_zip_upload.swift",
            "scripts/run_full_codebase_audit.sh",
            "scripts/selfcheck_absorb.sh",
            "scripts/selfcheck_finish_rules.sh",
            "scripts/selfcheck_generation_policy.sh",
            "scripts/selfcheck_longrun_policy.sh",
            "scripts/selfcheck_relay_policy.sh",
            "scripts/selfcheck_wake.sh",
        ]
        .into_iter()
        .collect();
        let actual: std::collections::BTreeSet<_> = BUNDLED_SKILL_ASSETS
            .iter()
            .filter_map(|(skill, path, _)| (*skill == "psst-gpt").then_some(*path))
            .collect();
        assert_eq!(actual, expected);
    }

    #[test]
    fn remove_legacy_is_noop_when_directory_does_not_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // No directory exists for the legacy name
        remove_legacy_skills(home, &["check"], BUNDLED_SKILLS);

        // Should not panic or create anything
        assert!(!home.join("skills/check").exists());
    }

    #[test]
    fn legacy_cleanup_runs_even_on_same_version_fast_path() {
        let tmp = tempfile::tempdir().unwrap();
        let home = tmp.path();

        // First run: extract current state
        extract_bundled_files(home);

        // Simulate user still having an old legacy directory
        let legacy_dir = home.join("skills/check");
        std::fs::create_dir_all(&legacy_dir).unwrap();
        std::fs::write(legacy_dir.join("SKILL.md"), "stale").unwrap();

        // Force the "same version" fast path by writing the current version marker
        let version = ds_version::VERSION;
        std::fs::write(home.join(".metadata_version"), version).unwrap();

        // This should still run legacy cleanup even though we're in fast path
        extract_bundled_files(home);

        assert!(
            !legacy_dir.exists(),
            "legacy cleanup must run even on same-version fast path"
        );
    }
}
