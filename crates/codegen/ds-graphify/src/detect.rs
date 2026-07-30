//! Corpus detection — collect supported files under a root.
//! Mirrors graphify `detect.py` categories and extensions.

use crate::schema::{DetectionFiles, DetectionResult};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "pyi", "go", "js", "jsx", "mjs", "cjs", "ts", "tsx", "mts", "cts", "java", "c",
    "h", "cpp", "cc", "cxx", "hpp", "rb", "cs", "kt", "kts", "scala", "php", "swift", "lua", "zig",
    "ex", "exs", "jl", "vue", "svelte", "astro", "dart", "sql", "sh", "bash", "json", "toml",
    "yaml", "yml",
];

const DOC_EXTENSIONS: &[&str] = &["md", "mdx", "qmd", "txt", "rst", "html"];
const PAPER_EXTENSIONS: &[&str] = &["pdf"];
const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mp3", "wav", "m4a", "webm"];

const SENSITIVE_NAMES: &[&str] = &[
    ".env",
    ".env.local",
    ".env.production",
    "id_rsa",
    "id_ed25519",
    "credentials.json",
    "secrets.yaml",
    "secrets.yml",
];

/// Detect all supported files under `root`.
///
/// Respects `.gitignore` via the `ignore` crate and merges `.graphifyignore`
/// patterns when present.
pub fn detect(root: &Path) -> anyhow::Result<DetectionResult> {
    detect_excluding(root, &[])
}

/// Detect supported files while excluding exact files or directory trees.
///
/// The pipeline uses this for a custom output directory so generated graph
/// artifacts can never become inputs to the next build.
pub fn detect_excluding(
    root: &Path,
    excluded_paths: &[PathBuf],
) -> anyhow::Result<DetectionResult> {
    let root = dunce_canonicalize(root)?;
    let excluded_paths: Vec<PathBuf> = excluded_paths
        .iter()
        .map(|path| dunce_canonicalize(path))
        .collect::<anyhow::Result<_>>()?;
    let mut builder = ignore::WalkBuilder::new(&root);
    builder
        .hidden(false)
        .git_ignore(true)
        .git_global(true)
        .git_exclude(true)
        .parents(true);

    // Merge .graphifyignore if present (extra excludes only).
    let gi = root.join(".graphifyignore");
    if gi.is_file() {
        builder.add_custom_ignore_filename(".graphifyignore");
    }

    let mut files = DetectionFiles::default();
    let mut skipped_sensitive = Vec::new();
    let mut total_words: u64 = 0;
    let mut seen: HashSet<PathBuf> = HashSet::new();

    for entry in builder.build() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let path = entry.path();
        if excluded_paths
            .iter()
            .any(|excluded| path == excluded || path.starts_with(excluded))
        {
            continue;
        }
        if should_skip_path(path, &root) {
            continue;
        }
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        if SENSITIVE_NAMES.iter().any(|s| name.eq_ignore_ascii_case(s))
            || name.ends_with(".pem")
            || name.ends_with(".key")
        {
            skipped_sensitive.push(name.to_string());
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if ext.is_empty() {
            continue;
        }
        let canon = path.to_path_buf();
        if !seen.insert(canon.clone()) {
            continue;
        }
        let rel = path_display(&canon, &root);
        let words = estimate_words(&canon);

        if CODE_EXTENSIONS.contains(&ext.as_str()) {
            // Docs disguised as yaml in DOC list handled separately.
            if matches!(ext.as_str(), "yaml" | "yml" | "toml" | "json") {
                // Keep as code/config for structural extract.
            }
            files.code.push(rel);
            total_words += words;
        } else if DOC_EXTENSIONS.contains(&ext.as_str()) {
            files.document.push(rel);
            total_words += words;
        } else if PAPER_EXTENSIONS.contains(&ext.as_str()) {
            files.paper.push(rel);
            total_words += words / 4; // rough
        } else if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
            files.image.push(rel);
        } else if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
            files.video.push(rel);
        }
    }

    let total_files = files.code.len()
        + files.document.len()
        + files.paper.len()
        + files.image.len()
        + files.video.len();

    let warning = if total_files > 500 || total_words > 2_000_000 {
        Some(format!(
            "Large corpus: {total_files} files · ~{total_words} words. Consider narrowing to a subfolder."
        ))
    } else {
        None
    };

    Ok(DetectionResult {
        scan_root: root.display().to_string(),
        total_files,
        total_words,
        warning,
        files,
        skipped_sensitive,
    })
}

fn should_skip_path(path: &Path, root: &Path) -> bool {
    let Ok(rel) = path.strip_prefix(root) else {
        return false;
    };
    for c in rel.components() {
        let s = c.as_os_str().to_string_lossy();
        if matches!(
            s.as_ref(),
            "graphify-out"
                | "node_modules"
                | "target"
                | ".git"
                | "dist"
                | "build"
                | ".venv"
                | "venv"
                | "__pycache__"
                | ".ds"
                | ".cargo"
        ) {
            return true;
        }
    }
    false
}

fn path_display(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
}

fn estimate_words(path: &Path) -> u64 {
    match std::fs::read_to_string(path) {
        Ok(s) => s.split_whitespace().count() as u64,
        Err(_) => 0,
    }
}

fn dunce_canonicalize(path: &Path) -> anyhow::Result<PathBuf> {
    let abs = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    Ok(dunce::canonicalize(&abs).unwrap_or(abs))
}

/// All absolute paths from a detection result, rooted at scan_root.
pub fn all_code_paths(det: &DetectionResult) -> Vec<PathBuf> {
    let root = PathBuf::from(&det.scan_root);
    det.files.code.iter().map(|r| root.join(r)).collect()
}

pub fn all_doc_paths(det: &DetectionResult) -> Vec<PathBuf> {
    let root = PathBuf::from(&det.scan_root);
    det.files.document.iter().map(|r| root.join(r)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn detects_rust_and_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.rs"), "fn main() {}\n").unwrap();
        std::fs::write(dir.path().join("README.md"), "# Hello\n\nworld\n").unwrap();
        let mut f = std::fs::File::create(dir.path().join(".env")).unwrap();
        writeln!(f, "SECRET=1").unwrap();

        let det = detect(dir.path()).unwrap();
        assert_eq!(det.files.code.len(), 1);
        assert_eq!(det.files.document.len(), 1);
        assert!(!det.skipped_sensitive.is_empty());
    }
}
