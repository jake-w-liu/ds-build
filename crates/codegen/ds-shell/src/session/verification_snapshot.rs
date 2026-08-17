//! Content-addressed workspace snapshots used by final verification.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub(crate) const SNAPSHOT_SCHEMA_VERSION: u32 = 1;
pub(crate) const SNAPSHOT_MANIFEST_PATH: &str = ".ds-verification/artifact-manifest.json";
const MAX_FILES: usize = 20_000;
const MAX_FILE_BYTES: u64 = 64 * 1024 * 1024;
const MAX_TOTAL_BYTES: u64 = 1024 * 1024 * 1024;
const MAX_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ArtifactKind {
    File,
    Symlink,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactEntry {
    pub path: String,
    pub kind: ArtifactKind,
    pub sha256: Option<String>,
    pub byte_len: u64,
    pub executable: bool,
    pub symlink_target: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArtifactManifest {
    pub schema_version: u32,
    pub scope_policy: String,
    pub entries: Vec<ArtifactEntry>,
    pub manifest_digest: String,
}

#[derive(Debug, Clone)]
pub(crate) struct VirtualArtifact {
    pub path: String,
    pub bytes: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct ReviewedSnapshot {
    pub root: PathBuf,
    pub manifest_path: PathBuf,
    pub manifest: ArtifactManifest,
}

pub(crate) fn digest_bytes(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}

pub(crate) fn capture_workspace_manifest(root: &Path) -> Result<ArtifactManifest, String> {
    let root = dunce::canonicalize(root)
        .map_err(|error| format!("cannot resolve workspace root: {error}"))?;
    let paths = scoped_paths(&root)?;
    if paths.len() > MAX_FILES {
        return Err(format!(
            "workspace snapshot exceeds the {MAX_FILES}-file limit"
        ));
    }

    let mut entries = Vec::with_capacity(paths.len());
    let mut total_bytes = 0_u64;
    for relative in paths {
        let full = root.join(&relative);
        let metadata = match fs::symlink_metadata(&full) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                entries.push(ArtifactEntry {
                    path: relative,
                    kind: ArtifactKind::Missing,
                    sha256: None,
                    byte_len: 0,
                    executable: false,
                    symlink_target: None,
                });
                continue;
            }
            Err(error) => return Err(format!("cannot inspect {relative}: {error}")),
        };
        if metadata.file_type().is_symlink() {
            let target = fs::read_link(&full)
                .map_err(|error| format!("cannot read symlink {relative}: {error}"))?;
            if target.is_absolute() {
                return Err(format!(
                    "absolute symlink {relative} cannot be frozen into a reviewed snapshot"
                ));
            }
            let resolved = dunce::canonicalize(&full)
                .map_err(|error| format!("cannot resolve symlink {relative}: {error}"))?;
            if !resolved.starts_with(&root) {
                return Err(format!("symlink {relative} escapes the workspace"));
            }
            entries.push(ArtifactEntry {
                path: relative,
                kind: ArtifactKind::Symlink,
                sha256: None,
                byte_len: 0,
                executable: false,
                symlink_target: Some(target.to_string_lossy().into_owned()),
            });
            continue;
        }
        if !metadata.is_file() {
            return Err(format!("scoped artifact {relative} is not a regular file"));
        }
        if metadata.len() > MAX_FILE_BYTES {
            return Err(format!(
                "artifact {relative} exceeds the {MAX_FILE_BYTES}-byte limit"
            ));
        }
        total_bytes = total_bytes.saturating_add(metadata.len());
        if total_bytes > MAX_TOTAL_BYTES {
            return Err(format!(
                "workspace snapshot exceeds the {MAX_TOTAL_BYTES}-byte total limit"
            ));
        }
        let bytes = read_regular_file(&full, MAX_FILE_BYTES)?;
        entries.push(ArtifactEntry {
            path: relative,
            kind: ArtifactKind::File,
            sha256: Some(digest_bytes(&bytes)),
            byte_len: bytes.len() as u64,
            executable: is_executable(&metadata),
            symlink_target: None,
        });
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut manifest = ArtifactManifest {
        schema_version: SNAPSHOT_SCHEMA_VERSION,
        scope_policy: "git tracked+untracked (ignored/build caches and secret-like files excluded)"
            .to_string(),
        entries,
        manifest_digest: String::new(),
    };
    manifest.manifest_digest = manifest_digest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn persist_manifest(path: &Path, manifest: &ArtifactManifest) -> Result<(), String> {
    validate_manifest(manifest)?;
    let body = serde_json::to_vec_pretty(manifest)
        .map_err(|error| format!("cannot serialize artifact manifest: {error}"))?;
    if body.len() as u64 > MAX_MANIFEST_BYTES {
        return Err("artifact manifest exceeds its size limit".to_string());
    }
    let body = String::from_utf8(body).expect("JSON serialization is UTF-8");
    crate::util::config::atomic_write_string(path, &body)
        .map_err(|error| format!("cannot persist artifact manifest: {error}"))
}

pub(crate) fn read_manifest(path: &Path) -> Result<ArtifactManifest, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("artifact manifest is missing: {error}"))?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_MANIFEST_BYTES {
        return Err("artifact manifest is not a bounded regular file".to_string());
    }
    let bytes = read_regular_file(path, MAX_MANIFEST_BYTES)?;
    let manifest: ArtifactManifest = serde_json::from_slice(&bytes)
        .map_err(|error| format!("artifact manifest is malformed: {error}"))?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

pub(crate) fn create_reviewed_snapshot(
    workspace_root: &Path,
    goal_scratch_root: &Path,
    round_id: &str,
    virtual_artifacts: &[VirtualArtifact],
) -> Result<ReviewedSnapshot, String> {
    if uuid::Uuid::parse_str(round_id).is_err() {
        return Err("verification round id is not a UUID".to_string());
    }
    let workspace_root = dunce::canonicalize(workspace_root)
        .map_err(|error| format!("cannot resolve workspace root: {error}"))?;
    let rounds_root = goal_scratch_root.join("verification-rounds");
    create_private_dir_all(&rounds_root)?;
    let round_root = rounds_root.join(round_id);
    fs::create_dir(&round_root)
        .map_err(|error| format!("cannot create fresh verification round: {error}"))?;
    set_dir_private(&round_root)?;
    let review_root = round_root.join("review");
    fs::create_dir(&review_root)
        .map_err(|error| format!("cannot create reviewed snapshot: {error}"))?;
    set_dir_private(&review_root)?;

    let mut manifest = capture_workspace_manifest(&workspace_root)?;
    for entry in &manifest.entries {
        copy_entry(&workspace_root, &review_root, entry)?;
    }

    let mut seen: BTreeSet<String> = manifest.entries.iter().map(|e| e.path.clone()).collect();
    for artifact in virtual_artifacts {
        validate_relative_path(&artifact.path)?;
        if artifact.path == SNAPSHOT_MANIFEST_PATH || !seen.insert(artifact.path.clone()) {
            return Err(format!("duplicate virtual artifact path {}", artifact.path));
        }
        let destination = review_root.join(&artifact.path);
        create_parent_dirs(&destination, &review_root)?;
        write_new_file(&destination, &artifact.bytes)?;
        manifest.entries.push(ArtifactEntry {
            path: artifact.path.clone(),
            kind: ArtifactKind::File,
            sha256: Some(digest_bytes(&artifact.bytes)),
            byte_len: artifact.bytes.len() as u64,
            executable: false,
            symlink_target: None,
        });
    }
    manifest
        .entries
        .sort_by(|left, right| left.path.cmp(&right.path));
    manifest.manifest_digest.clear();
    manifest.manifest_digest = manifest_digest(&manifest)?;
    let manifest_path = review_root.join(SNAPSHOT_MANIFEST_PATH);
    create_parent_dirs(&manifest_path, &review_root)?;
    let manifest_bytes = serde_json::to_vec_pretty(&manifest)
        .map_err(|error| format!("cannot serialize snapshot manifest: {error}"))?;
    write_new_file(&manifest_path, &manifest_bytes)?;
    freeze_tree(&review_root)?;
    verify_reviewed_snapshot(&review_root, &manifest)?;
    Ok(ReviewedSnapshot {
        root: review_root,
        manifest_path,
        manifest,
    })
}

pub(crate) fn verify_reviewed_snapshot(
    root: &Path,
    manifest: &ArtifactManifest,
) -> Result<(), String> {
    validate_manifest(manifest)?;
    let root = dunce::canonicalize(root)
        .map_err(|error| format!("cannot resolve reviewed snapshot: {error}"))?;
    let expected: BTreeMap<_, _> = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let mut observed = BTreeSet::new();
    for item in walkdir::WalkDir::new(&root).follow_links(false) {
        let item = item.map_err(|error| format!("cannot walk reviewed snapshot: {error}"))?;
        if item.path() == root || item.file_type().is_dir() {
            continue;
        }
        let relative = relative_utf8(&root, item.path())?;
        if relative == SNAPSHOT_MANIFEST_PATH {
            continue;
        }
        let Some(entry) = expected.get(relative.as_str()) else {
            return Err(format!(
                "reviewed snapshot contains unexpected artifact {relative}"
            ));
        };
        observed.insert(relative.clone());
        verify_entry(&root, entry)?;
    }
    for entry in &manifest.entries {
        match entry.kind {
            ArtifactKind::Missing => {
                if fs::symlink_metadata(root.join(&entry.path)).is_ok() {
                    return Err(format!(
                        "expected-missing artifact {} now exists",
                        entry.path
                    ));
                }
            }
            _ if !observed.contains(&entry.path) => {
                return Err(format!("reviewed artifact {} is missing", entry.path));
            }
            _ => {}
        }
    }
    let on_disk_manifest = read_manifest(&root.join(SNAPSHOT_MANIFEST_PATH))?;
    if &on_disk_manifest != manifest {
        return Err("reviewed snapshot manifest changed".to_string());
    }
    Ok(())
}

pub(crate) fn changed_paths(
    baseline: &ArtifactManifest,
    current: &ArtifactManifest,
) -> Result<Vec<String>, String> {
    validate_manifest(baseline)?;
    validate_manifest(current)?;
    let before: BTreeMap<_, _> = baseline
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let after: BTreeMap<_, _> = current
        .entries
        .iter()
        .filter(|entry| !entry.path.starts_with(".ds-verification/"))
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    Ok(before
        .keys()
        .chain(after.keys())
        .copied()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|path| before.get(path) != after.get(path))
        .map(str::to_string)
        .collect())
}

/// Normalize a receipt-cited sha256 to the canonical `sha256:<64 hex>` form.
///
/// Accepts the bare 64-hex form as well: skeptic models reliably strip the
/// prefix when transcribing a manifest digest, and rejecting that with an
/// opaque "unknown artifact revision" was a recurring harness friction.
/// Anything else (wrong length, non-hex, other prefixes) is rejected.
pub(crate) fn normalize_sha256(raw: &str) -> Option<String> {
    let trimmed = raw.trim();
    let hex = trimmed
        .strip_prefix("sha256:")
        .or_else(|| trimmed.strip_prefix("SHA256:"))
        .unwrap_or(trimmed);
    if hex.len() == 64 && hex.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        Some(format!("sha256:{}", hex.to_ascii_lowercase()))
    } else {
        None
    }
}

/// Validate a receipt's artifact citation against the manifest with a
/// distinct, actionable reason for every failure mode an implementer can
/// hit, instead of one opaque "unknown artifact revision" for all of them:
///
/// - the cited path is not part of the reviewed manifest at all (the usual
///   cause: citing an absolute scratch-dir path or a file created after the
///   round's manifest snapshot — both are invisible to the reviewer),
/// - the path exists but is not a regular-file entry,
/// - the cited digest is malformed, or
/// - the digest is well-formed but does not match the manifest revision
///   (stale file: the artifact changed after the snapshot).
pub(crate) fn match_entry_diagnostic(
    manifest: &ArtifactManifest,
    path: &str,
    sha256: &str,
) -> Result<(), String> {
    let entry = manifest.entries.iter().find(|entry| entry.path == path);
    let Some(entry) = entry else {
        return Err(format!(
            "cites artifact `{path}` which is not part of the reviewed manifest{}",
            manifest_hint(manifest, path)
        ));
    };
    if entry.kind != ArtifactKind::File {
        return Err(format!(
            "cites artifact `{path}` which is not a regular file in the reviewed manifest"
        ));
    }
    let Some(cited) = normalize_sha256(sha256) else {
        return Err(format!(
            "has a malformed artifact digest for `{path}` \
             (expected `sha256:<64 hex>` or a bare 64-hex digest; got `{}`)",
            ellipsize(sha256.trim(), 80)
        ));
    };
    let recorded = entry.sha256.as_deref().unwrap_or_default();
    if !recorded.eq_ignore_ascii_case(&cited) {
        return Err(format!(
            "cites a stale or unknown revision of `{path}` \
             (cited {cited}, manifest records {recorded})"
        ));
    }
    Ok(())
}

/// Suggest the manifest paths closest to a missing citation so the skeptic
/// (and through it the implementer) can pick a real one next round.
fn manifest_hint(manifest: &ArtifactManifest, path: &str) -> String {
    const HINT_COUNT: usize = 3;
    let mut scored: Vec<(&str, usize)> = manifest
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), shared_prefix_len(&entry.path, path)))
        .collect();
    scored.sort_by_key(|(_, score)| std::cmp::Reverse(*score));
    let names: Vec<String> = scored
        .iter()
        .take(HINT_COUNT)
        .map(|(candidate, _)| format!("`{candidate}`"))
        .collect();
    if names.is_empty() {
        String::new()
    } else {
        format!("; nearest manifest paths: {}", names.join(", "))
    }
}

fn shared_prefix_len(a: &str, b: &str) -> usize {
    a.chars().zip(b.chars()).take_while(|(x, y)| x == y).count()
}

fn ellipsize(text: &str, max_chars: usize) -> String {
    if text.chars().count() > max_chars {
        let cut: String = text.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{cut}…")
    } else {
        text.to_string()
    }
}

fn validate_manifest(manifest: &ArtifactManifest) -> Result<(), String> {
    if manifest.schema_version != SNAPSHOT_SCHEMA_VERSION {
        return Err(format!(
            "unsupported artifact manifest schema version {}",
            manifest.schema_version
        ));
    }
    if manifest.entries.len() > MAX_FILES + 16 || manifest.scope_policy.trim().is_empty() {
        return Err("artifact manifest is incomplete or oversized".to_string());
    }
    let mut paths = BTreeSet::new();
    for entry in &manifest.entries {
        validate_relative_path(&entry.path)?;
        if !paths.insert(entry.path.as_str()) {
            return Err(format!("duplicate artifact path {}", entry.path));
        }
        match entry.kind {
            ArtifactKind::File => {
                if entry
                    .sha256
                    .as_deref()
                    .is_none_or(|value| !valid_digest(value))
                    || entry.symlink_target.is_some()
                {
                    return Err(format!("file artifact {} has invalid metadata", entry.path));
                }
            }
            ArtifactKind::Symlink => {
                if entry.sha256.is_some()
                    || entry.symlink_target.as_deref().is_none_or(str::is_empty)
                {
                    return Err(format!(
                        "symlink artifact {} has invalid metadata",
                        entry.path
                    ));
                }
            }
            ArtifactKind::Missing => {
                if entry.sha256.is_some() || entry.symlink_target.is_some() || entry.byte_len != 0 {
                    return Err(format!(
                        "missing artifact {} has invalid metadata",
                        entry.path
                    ));
                }
            }
        }
    }
    if !valid_digest(&manifest.manifest_digest)
        || manifest_digest(manifest)? != manifest.manifest_digest
    {
        return Err("artifact manifest digest mismatch".to_string());
    }
    Ok(())
}

fn manifest_digest(manifest: &ArtifactManifest) -> Result<String, String> {
    let mut digestable = manifest.clone();
    digestable.manifest_digest.clear();
    serde_json::to_vec(&digestable)
        .map(|bytes| digest_bytes(&bytes))
        .map_err(|error| format!("cannot serialize artifact manifest: {error}"))
}

fn scoped_paths(root: &Path) -> Result<Vec<String>, String> {
    let output = std::process::Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "-z",
            "--cached",
            "--others",
            "--exclude-standard",
        ])
        .output();
    if let Ok(output) = output
        && output.status.success()
    {
        let text = String::from_utf8(output.stdout)
            .map_err(|_| "git returned a non-UTF-8 artifact path".to_string())?;
        let mut paths = text
            .split('\0')
            .filter(|path| !path.is_empty() && !excluded_path(path))
            .map(str::to_string)
            .collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        for path in &paths {
            validate_relative_path(path)?;
        }
        return Ok(paths);
    }

    let mut paths = Vec::new();
    let walker = walkdir::WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_entry(|entry| {
            entry.path() == root
                || relative_utf8(root, entry.path()).is_ok_and(|path| !excluded_path(&path))
        });
    for item in walker {
        let item = item.map_err(|error| format!("cannot walk workspace: {error}"))?;
        if item.path() == root || item.file_type().is_dir() {
            continue;
        }
        paths.push(relative_utf8(root, item.path())?);
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn excluded_path(path: &str) -> bool {
    let normalized = path.replace('\\', "/");
    let components: Vec<_> = normalized.split('/').collect();
    if components.iter().any(|part| {
        matches!(
            *part,
            ".git" | ".ds" | ".codex" | "target" | "node_modules" | ".venv" | "__pycache__"
        )
    }) {
        return true;
    }
    let name = components
        .last()
        .copied()
        .unwrap_or_default()
        .to_ascii_lowercase();
    name == ".env"
        || name.ends_with(".pem")
        || name.ends_with(".key")
        || name.contains("credentials")
}

fn copy_entry(
    source_root: &Path,
    destination_root: &Path,
    entry: &ArtifactEntry,
) -> Result<(), String> {
    let destination = destination_root.join(&entry.path);
    match entry.kind {
        ArtifactKind::Missing => Ok(()),
        ArtifactKind::File => {
            create_parent_dirs(&destination, destination_root)?;
            let bytes = read_regular_file(&source_root.join(&entry.path), MAX_FILE_BYTES)?;
            if bytes.len() as u64 != entry.byte_len
                || entry.sha256.as_deref() != Some(digest_bytes(&bytes).as_str())
            {
                return Err(format!("artifact {} changed during snapshot", entry.path));
            }
            write_new_file(&destination, &bytes)?;
            set_file_pre_freeze_permissions(&destination, entry.executable)
        }
        ArtifactKind::Symlink => {
            create_parent_dirs(&destination, destination_root)?;
            let target = entry
                .symlink_target
                .as_deref()
                .ok_or_else(|| format!("symlink {} has no target", entry.path))?;
            create_symlink(Path::new(target), &destination)
                .map_err(|error| format!("cannot copy symlink {}: {error}", entry.path))
        }
    }
}

fn verify_entry(root: &Path, entry: &ArtifactEntry) -> Result<(), String> {
    let path = root.join(&entry.path);
    let metadata = fs::symlink_metadata(&path)
        .map_err(|error| format!("cannot inspect reviewed artifact {}: {error}", entry.path))?;
    match entry.kind {
        ArtifactKind::File if metadata.file_type().is_file() => {
            let bytes = read_regular_file(&path, MAX_FILE_BYTES)?;
            if bytes.len() as u64 == entry.byte_len
                && entry.sha256.as_deref() == Some(digest_bytes(&bytes).as_str())
                && is_executable(&metadata) == entry.executable
            {
                Ok(())
            } else {
                Err(format!("reviewed artifact {} changed", entry.path))
            }
        }
        ArtifactKind::Symlink if metadata.file_type().is_symlink() => {
            let target = fs::read_link(&path)
                .map_err(|error| format!("cannot inspect symlink {}: {error}", entry.path))?;
            if entry.symlink_target.as_deref() == target.to_str() {
                Ok(())
            } else {
                Err(format!("reviewed symlink {} changed", entry.path))
            }
        }
        ArtifactKind::Missing => Err(format!("expected-missing artifact {} exists", entry.path)),
        _ => Err(format!("reviewed artifact {} changed type", entry.path)),
    }
}

fn create_parent_dirs(path: &Path, root: &Path) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| "artifact path has no parent".to_string())?;
    if !parent.starts_with(root) {
        return Err("artifact path escaped the reviewed root".to_string());
    }
    fs::create_dir_all(parent).map_err(|error| format!("cannot create snapshot directory: {error}"))
}

fn write_new_file(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    file.write_all(bytes)
        .map_err(|error| format!("cannot write {}: {error}", path.display()))?;
    file.sync_all()
        .map_err(|error| format!("cannot sync {}: {error}", path.display()))
}

fn read_regular_file(path: &Path, limit: u64) -> Result<Vec<u8>, String> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
    if !metadata.file_type().is_file() || metadata.len() > limit {
        return Err(format!("{} is not a bounded regular file", path.display()));
    }
    let file = open_read_no_follow(path)
        .map_err(|error| format!("cannot open {} safely: {error}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("cannot read {}: {error}", path.display()))?;
    if bytes.len() as u64 != metadata.len() {
        return Err(format!("{} changed while being read", path.display()));
    }
    Ok(bytes)
}

#[cfg(unix)]
fn open_read_no_follow(path: &Path) -> std::io::Result<File> {
    use std::os::unix::fs::OpenOptionsExt;
    OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path)
}

#[cfg(not(unix))]
fn open_read_no_follow(path: &Path) -> std::io::Result<File> {
    OpenOptions::new().read(true).open(path)
}

fn validate_relative_path(path: &str) -> Result<(), String> {
    let path = Path::new(path);
    if path.as_os_str().is_empty()
        || path.is_absolute()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("unsafe artifact path {}", path.display()));
    }
    Ok(())
}

fn relative_utf8(root: &Path, path: &Path) -> Result<String, String> {
    path.strip_prefix(root)
        .map_err(|_| "artifact path escaped the workspace".to_string())?
        .to_str()
        .map(str::to_string)
        .ok_or_else(|| "workspace contains a non-UTF-8 artifact path".to_string())
}

fn valid_digest(value: &str) -> bool {
    value.len() == 71
        && value
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

#[cfg(unix)]
fn is_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::windows::fs::symlink_file(target, link)
}

fn create_private_dir_all(path: &Path) -> Result<(), String> {
    fs::create_dir_all(path)
        .map_err(|error| format!("cannot create {}: {error}", path.display()))?;
    set_dir_private(path)
}

#[cfg(unix)]
fn set_dir_private(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))
        .map_err(|error| format!("cannot secure {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_dir_private(_path: &Path) -> Result<(), String> {
    Ok(())
}

fn freeze_tree(root: &Path) -> Result<(), String> {
    let mut directories = Vec::new();
    for item in walkdir::WalkDir::new(root).follow_links(false) {
        let item = item.map_err(|error| format!("cannot freeze reviewed snapshot: {error}"))?;
        let metadata = fs::symlink_metadata(item.path())
            .map_err(|error| format!("cannot inspect reviewed snapshot: {error}"))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            directories.push(item.path().to_path_buf());
        } else if metadata.is_file() {
            set_file_read_only(item.path(), is_executable(&metadata))?;
        }
    }
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        set_dir_read_only(&directory)?;
    }
    Ok(())
}

#[cfg(unix)]
fn set_file_read_only(path: &Path, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o555 } else { 0o444 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot freeze {}: {error}", path.display()))
}

#[cfg(unix)]
fn set_file_pre_freeze_permissions(path: &Path, executable: bool) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if executable { 0o700 } else { 0o600 };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
        .map_err(|error| format!("cannot preserve mode for {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_file_pre_freeze_permissions(_path: &Path, _executable: bool) -> Result<(), String> {
    Ok(())
}

#[cfg(unix)]
fn set_dir_read_only(path: &Path) -> Result<(), String> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o555))
        .map_err(|error| format!("cannot freeze {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_file_read_only(path: &Path, _executable: bool) -> Result<(), String> {
    let mut permissions = fs::metadata(path)
        .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?
        .permissions();
    permissions.set_readonly(true);
    fs::set_permissions(path, permissions)
        .map_err(|error| format!("cannot freeze {}: {error}", path.display()))
}

#[cfg(not(unix))]
fn set_dir_read_only(_path: &Path) -> Result<(), String> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_manifest() -> ArtifactManifest {
        ArtifactManifest {
            schema_version: SNAPSHOT_SCHEMA_VERSION,
            scope_policy: "test".to_string(),
            manifest_digest: "sha256:00".repeat(4),
            entries: vec![
                ArtifactEntry {
                    path: "sheet.tex".to_string(),
                    kind: ArtifactKind::File,
                    sha256: Some(digest_bytes(b"sheet v1")),
                    byte_len: 8,
                    executable: false,
                    symlink_target: None,
                },
                ArtifactEntry {
                    path: "verification/run.out".to_string(),
                    kind: ArtifactKind::File,
                    sha256: Some(digest_bytes(b"run out")),
                    byte_len: 7,
                    executable: false,
                    symlink_target: None,
                },
                ArtifactEntry {
                    path: "notes.md".to_string(),
                    kind: ArtifactKind::Missing,
                    sha256: None,
                    byte_len: 0,
                    executable: false,
                    symlink_target: None,
                },
            ],
        }
    }

    #[test]
    fn normalize_sha256_accepts_both_digest_forms() {
        let bare = "0f".repeat(32);
        assert_eq!(
            normalize_sha256(&bare).as_deref(),
            Some(format!("sha256:{bare}").as_str())
        );
        assert_eq!(
            normalize_sha256(&format!("sha256:{bare}")).as_deref(),
            Some(format!("sha256:{bare}").as_str())
        );
        assert_eq!(
            normalize_sha256(&format!("SHA256:{}", bare.to_ascii_uppercase())).as_deref(),
            Some(format!("sha256:{bare}").as_str())
        );
        assert!(normalize_sha256("sha256:beef").is_none(), "short digest");
        assert!(normalize_sha256("").is_none());
        assert!(normalize_sha256(&("md5:".to_string() + &bare)).is_none());
    }

    #[test]
    fn match_entry_diagnostic_distinguishes_failure_modes() {
        let manifest = sample_manifest();
        let recorded = digest_bytes(b"sheet v1");

        // Unknown path → names the path and hints at real manifest entries.
        let err = match_entry_diagnostic(&manifest, "/tmp/scratch/evidence.out", &recorded)
            .unwrap_err();
        assert!(err.contains("not part of the reviewed manifest"), "{err}");
        assert!(err.contains("nearest manifest paths"), "{err}");
        assert!(err.contains("`sheet.tex`"), "{err}");

        // Non-file entry → distinct message.
        let err = match_entry_diagnostic(&manifest, "notes.md", &recorded).unwrap_err();
        assert!(err.contains("not a regular file"), "{err}");

        // Malformed digest → shows the bad value, bounded.
        let err = match_entry_diagnostic(&manifest, "sheet.tex", "not-a-digest").unwrap_err();
        assert!(err.contains("malformed artifact digest"), "{err}");

        // Well-formed but stale → shows cited vs recorded digests.
        let stale = digest_bytes(b"sheet v2");
        let err = match_entry_diagnostic(&manifest, "sheet.tex", &stale).unwrap_err();
        assert!(err.contains("stale or unknown revision"), "{err}");
        assert!(err.contains("manifest records"), "{err}");

        // Matching revisions (both digest forms) pass.
        assert!(match_entry_diagnostic(&manifest, "sheet.tex", &recorded).is_ok());
        assert!(match_entry_diagnostic(
            &manifest,
            "sheet.tex",
            recorded.trim_start_matches("sha256:")
        )
        .is_ok());
    }

    #[test]
    fn manifest_captures_dirty_untracked_missing_and_spaces() {
        let dir = tempfile::tempdir().unwrap();
        std::process::Command::new("git")
            .args(["init", "-q"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        fs::write(dir.path().join("tracked.txt"), "before").unwrap();
        fs::write(dir.path().join("deleted.txt"), "gone").unwrap();
        std::process::Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .status()
            .unwrap();
        fs::write(dir.path().join("tracked.txt"), "dirty").unwrap();
        fs::remove_file(dir.path().join("deleted.txt")).unwrap();
        fs::write(dir.path().join("input with spaces.tex"), "derive x").unwrap();

        let manifest = capture_workspace_manifest(dir.path()).unwrap();
        let map: BTreeMap<_, _> = manifest
            .entries
            .iter()
            .map(|entry| (entry.path.as_str(), entry))
            .collect();
        assert_eq!(
            map["tracked.txt"].sha256.as_deref(),
            Some(digest_bytes(b"dirty").as_str())
        );
        assert_eq!(map["deleted.txt"].kind, ArtifactKind::Missing);
        assert!(map.contains_key("input with spaces.tex"));
    }

    #[test]
    fn snapshot_is_content_bound_and_detects_mutation() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("answer.txt"), "42").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let snapshot = create_reviewed_snapshot(
            workspace.path(),
            scratch.path(),
            &uuid::Uuid::now_v7().to_string(),
            &[VirtualArtifact {
                path: ".ds-verification/final-response.md".to_string(),
                bytes: b"done".to_vec(),
            }],
        )
        .unwrap();
        verify_reviewed_snapshot(&snapshot.root, &snapshot.manifest).unwrap();
        make_writable_for_test(&snapshot.root.join("answer.txt"));
        fs::write(snapshot.root.join("answer.txt"), "wrong").unwrap();
        assert!(verify_reviewed_snapshot(&snapshot.root, &snapshot.manifest).is_err());
    }

    #[cfg(unix)]
    fn make_writable_for_test(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    #[cfg(not(unix))]
    fn make_writable_for_test(path: &Path) {
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[test]
    fn two_rounds_use_distinct_namespaces() {
        let workspace = tempfile::tempdir().unwrap();
        fs::write(workspace.path().join("a.txt"), "a").unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let first = create_reviewed_snapshot(
            workspace.path(),
            scratch.path(),
            &uuid::Uuid::now_v7().to_string(),
            &[],
        )
        .unwrap();
        let second = create_reviewed_snapshot(
            workspace.path(),
            scratch.path(),
            &uuid::Uuid::now_v7().to_string(),
            &[],
        )
        .unwrap();
        assert_ne!(first.root, second.root);
        assert_eq!(
            first.manifest.manifest_digest,
            second.manifest.manifest_digest
        );
    }

    #[cfg(unix)]
    #[test]
    fn snapshot_preserves_executable_identity() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().unwrap();
        let script = workspace.path().join("check.sh");
        fs::write(&script, "#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
        let scratch = tempfile::tempdir().unwrap();
        let snapshot = create_reviewed_snapshot(
            workspace.path(),
            scratch.path(),
            &uuid::Uuid::now_v7().to_string(),
            &[],
        )
        .unwrap();

        let copied = snapshot.root.join("check.sh");
        assert!(is_executable(&fs::metadata(&copied).unwrap()));
        verify_reviewed_snapshot(&snapshot.root, &snapshot.manifest).unwrap();
    }
}
