//! Markdown / docs structural extraction (no LLM).
//! Headings, wiki links, markdown links — Graphify-compatible.

use crate::ids::make_id;
use crate::schema::{Confidence, Edge, Extraction, FileType, Node};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*$").unwrap());
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap());
static WIKI_LINK_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());
static RATIONALE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?://|#|--)\s*(NOTE|WHY|HACK|TODO|FIXME):\s*(.+)$").unwrap()
});

pub fn extract_markdown(path: &Path, source_key: &str) -> Extraction {
    let source = match std::fs::read_to_string(path) {
        Ok(s) => s,
        Err(e) => {
            return Extraction {
                error: Some(e.to_string()),
                ..Extraction::empty()
            };
        }
    };

    let path_str = source_key.replace('\\', "/");
    let file_nid = make_id(&[&path_str]);
    let file_name = Path::new(&path_str)
        .file_name()
        .and_then(|n| n.to_str())
        .or_else(|| path.file_name().and_then(|n| n.to_str()))
        .unwrap_or("doc")
        .to_string();

    let mut nodes = vec![Node {
        id: file_nid.clone(),
        label: file_name,
        file_type: FileType::Document,
        source_file: path_str.clone(),
        source_location: Some("L1".into()),
        community: None,
        origin_file: None,
    }];
    let mut edges = Vec::new();
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    seen.insert(file_nid.clone());

    let mut heading_stack: Vec<(usize, String)> = Vec::new();
    let mut in_code = false;
    let mut linked: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (i, line) in source.lines().enumerate() {
        let line_num = i + 1;
        let trimmed = line.trim();
        if trimmed.starts_with("```") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            continue;
        }

        if let Some(cap) = HEADING_RE.captures(line) {
            let level = cap[1].len();
            let title = cap[2].trim().to_string();
            while heading_stack.last().is_some_and(|(l, _)| *l >= level) {
                heading_stack.pop();
            }
            let parent = heading_stack
                .last()
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| file_nid.clone());
            let base_id = make_id(&[&parent, &title]);
            let mut hid = base_id.clone();
            let mut occurrence = 2usize;
            while seen.contains(&hid) {
                hid = make_id(&[&base_id, &occurrence.to_string()]);
                occurrence += 1;
            }
            seen.insert(hid.clone());
            nodes.push(Node {
                id: hid.clone(),
                label: title.clone(),
                file_type: FileType::Document,
                source_file: path_str.clone(),
                source_location: Some(format!("L{line_num}")),
                community: None,
                origin_file: None,
            });
            edges.push(Edge {
                source: parent,
                target: hid.clone(),
                relation: "contains".into(),
                confidence: Confidence::Extracted,
                source_file: path_str.clone(),
                source_location: Some(format!("L{line_num}")),
                weight: Some(1.0),
                context: None,
            });
            heading_stack.push((level, hid));
        }

        for cap in MD_LINK_RE.captures_iter(line) {
            let target = cap[2].trim();
            add_doc_link(
                target,
                path,
                &path_str,
                &file_nid,
                line_num,
                &path_str,
                &mut nodes,
                &mut edges,
                &mut seen,
                &mut linked,
            );
        }
        for cap in WIKI_LINK_RE.captures_iter(line) {
            let target = cap[1].split('|').next().unwrap_or(&cap[1]).trim();
            let t = if target.contains('.') {
                target.to_string()
            } else {
                format!("{target}.md")
            };
            add_doc_link(
                &t,
                path,
                &path_str,
                &file_nid,
                line_num,
                &path_str,
                &mut nodes,
                &mut edges,
                &mut seen,
                &mut linked,
            );
        }
    }

    Extraction {
        nodes,
        edges,
        error: None,
        input_tokens: 0,
        output_tokens: 0,
    }
}

fn add_doc_link(
    raw: &str,
    source_path: &Path,
    source_key: &str,
    file_nid: &str,
    line: usize,
    path_str: &str,
    nodes: &mut Vec<Node>,
    edges: &mut Vec<Edge>,
    seen: &mut std::collections::HashSet<String>,
    linked: &mut std::collections::HashSet<String>,
) {
    let target = raw.split(['#', '?']).next().unwrap_or(raw).trim();
    if target.is_empty()
        || target.contains("://")
        || target.starts_with("mailto:")
        || target.starts_with("data:")
    {
        return;
    }
    let portable_target = target.replace('\\', "/");
    if portable_target.starts_with('/')
        || portable_target
            .as_bytes()
            .get(1)
            .is_some_and(|byte| *byte == b':')
    {
        return;
    }
    // Prefer a portable relative key (sibling of source_key) so the edge
    // target id matches the linked file's own node id after extract_many.
    let rel_key = {
        let parent = Path::new(source_key)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let joined = if parent.is_empty() {
            portable_target.clone()
        } else {
            format!("{parent}/{portable_target}")
        };
        // Normalize ./ and // without requiring the target to exist on disk.
        normalize_rel_path(&joined)
    };
    if rel_key == ".." || rel_key.starts_with("../") {
        return;
    }
    let tgt_nid = make_id(&[&rel_key]);
    if tgt_nid == file_nid || !linked.insert(tgt_nid.clone()) {
        return;
    }
    if seen.insert(tgt_nid.clone()) {
        let disk_path = source_path
            .parent()
            .unwrap_or_else(|| Path::new(""))
            .join(&portable_target);
        let exists = disk_path.is_file();
        let label = Path::new(&rel_key)
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or(&rel_key)
            .to_string();
        nodes.push(Node {
            id: tgt_nid.clone(),
            label,
            file_type: linked_file_type(&rel_key),
            source_file: if exists {
                rel_key.clone()
            } else {
                String::new()
            },
            source_location: None,
            community: None,
            origin_file: (!exists).then(|| path_str.to_string()),
        });
    }
    edges.push(Edge {
        source: file_nid.to_string(),
        target: tgt_nid,
        relation: "references".into(),
        confidence: Confidence::Extracted,
        source_file: path_str.to_string(),
        source_location: Some(format!("L{line}")),
        weight: Some(1.0),
        context: None,
    });
}

fn linked_file_type(path: &str) -> FileType {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "md" | "mdx" | "qmd" | "txt" | "rst" | "html" => FileType::Document,
        "pdf" => FileType::Paper,
        "png" | "jpg" | "jpeg" | "webp" | "gif" => FileType::Image,
        "rs" | "py" | "pyi" | "go" | "js" | "jsx" | "mjs" | "cjs" | "ts" | "tsx" | "mts"
        | "cts" | "java" | "c" | "h" | "cpp" | "cc" | "cxx" | "hpp" | "rb" | "cs" | "kt"
        | "kts" | "scala" | "php" | "swift" | "lua" | "zig" | "ex" | "exs" | "jl" | "vue"
        | "svelte" | "astro" | "dart" | "sql" | "sh" | "bash" | "json" | "toml" | "yaml"
        | "yml" => FileType::Code,
        _ => FileType::Concept,
    }
}

fn normalize_rel_path(p: &str) -> String {
    let normalized = p.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                if out.last().is_some_and(|last| *last != "..") {
                    out.pop();
                } else {
                    out.push("..");
                }
            }
            other => out.push(other),
        }
    }
    out.join("/")
}

/// Lightweight config file node (package manifests etc.).
pub fn extract_config_stub(path: &Path, source_key: &str) -> Extraction {
    let path_str = source_key.replace('\\', "/");
    let file_nid = make_id(&[&path_str]);
    let name = Path::new(&path_str)
        .file_name()
        .and_then(|n| n.to_str())
        .or_else(|| path.file_name().and_then(|n| n.to_str()))
        .unwrap_or("config")
        .to_string();
    let mut nodes = vec![Node {
        id: file_nid.clone(),
        label: name,
        file_type: FileType::Code,
        source_file: path_str.clone(),
        source_location: Some("L1".into()),
        community: None,
        origin_file: None,
    }];
    let mut edges = Vec::new();

    // Rationale comments from nearby source-like text in toml/yaml
    if let Ok(text) = std::fs::read_to_string(path) {
        for (i, line) in text.lines().enumerate() {
            if let Some(cap) = RATIONALE_RE.captures(line) {
                let kind = cap[1].to_ascii_uppercase();
                let body = cap[2].trim();
                let label = format!("{kind}: {body}");
                let nid = make_id(&[&file_nid, &kind, &format!("{}", i + 1)]);
                nodes.push(Node {
                    id: nid.clone(),
                    label: label.clone(),
                    file_type: FileType::Rationale,
                    source_file: path_str.clone(),
                    source_location: Some(format!("L{}", i + 1)),
                    community: None,
                    origin_file: None,
                });
                edges.push(Edge {
                    source: file_nid.clone(),
                    target: nid,
                    relation: "documents".into(),
                    confidence: Confidence::Extracted,
                    source_file: path_str.clone(),
                    source_location: Some(format!("L{}", i + 1)),
                    weight: Some(1.0),
                    context: None,
                });
            }
        }
        // Cargo.toml package identity.
        if path.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml") {
            let mut section = "";
            for (line_index, line) in text.lines().enumerate() {
                let t = line.trim();
                if t.starts_with('[') && t.ends_with(']') {
                    section = t;
                    continue;
                }
                if section != "[package]" {
                    continue;
                }
                let Some((key, value)) = t.split_once('=') else {
                    continue;
                };
                if key.trim() != "name" {
                    continue;
                }
                let value = value.trim();
                let package_name = value
                    .strip_prefix('"')
                    .and_then(|rest| rest.split('"').next())
                    .or_else(|| {
                        value
                            .strip_prefix('\'')
                            .and_then(|rest| rest.split('\'').next())
                    })
                    .unwrap_or("");
                if package_name.is_empty() {
                    continue;
                }
                let nid = make_id(&[package_name]);
                let source_location = Some(format!("L{}", line_index + 1));
                nodes.push(Node {
                    id: nid.clone(),
                    label: package_name.to_string(),
                    file_type: FileType::Code,
                    source_file: path_str.clone(),
                    source_location: source_location.clone(),
                    community: None,
                    origin_file: None,
                });
                edges.push(Edge {
                    source: file_nid.clone(),
                    target: nid,
                    relation: "contains".into(),
                    confidence: Confidence::Extracted,
                    source_file: path_str.clone(),
                    source_location,
                    weight: Some(1.0),
                    context: None,
                });
                break;
            }
        }
    }

    Extraction {
        nodes,
        edges,
        error: None,
        input_tokens: 0,
        output_tokens: 0,
    }
}
