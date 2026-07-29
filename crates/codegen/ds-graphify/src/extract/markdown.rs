//! Markdown / docs structural extraction (no LLM).
//! Headings, wiki links, markdown links — Graphify-compatible.

use crate::ids::make_id;
use crate::schema::{Confidence, Extraction, FileType, Node, Edge};
use regex::Regex;
use std::path::Path;
use std::sync::LazyLock;

static HEADING_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(#{1,6})\s+(.+?)\s*$").unwrap());
static MD_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[([^\]]*)\]\(([^)]+)\)").unwrap());
static WIKI_LINK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[\[([^\]]+)\]\]").unwrap());
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
            let hid = make_id(&[&file_nid, &title]);
            if seen.insert(hid.clone()) {
                nodes.push(Node {
                    id: hid.clone(),
                    label: title.clone(),
                    file_type: FileType::Document,
                    source_file: path_str.clone(),
                    source_location: Some(format!("L{line_num}")),
                    community: None,
                    origin_file: None,
                });
            }
            while heading_stack
                .last()
                .is_some_and(|(l, _)| *l >= level)
            {
                heading_stack.pop();
            }
            let parent = heading_stack
                .last()
                .map(|(_, id)| id.clone())
                .unwrap_or_else(|| file_nid.clone());
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
                &mut edges,
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
                &mut edges,
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
    edges: &mut Vec<Edge>,
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
    // Prefer a portable relative key (sibling of source_key) so the edge
    // target id matches the linked file's own node id after extract_many.
    let rel_key = if Path::new(target).is_absolute() {
        target.replace('\\', "/")
    } else {
        let parent = Path::new(source_key)
            .parent()
            .map(|p| p.display().to_string())
            .unwrap_or_default();
        let joined = if parent.is_empty() {
            target.to_string()
        } else {
            format!("{parent}/{target}")
        };
        // Normalize ./ and // without requiring the target to exist on disk.
        normalize_rel_path(&joined)
    };
    let _ = source_path; // fs path reserved for existence checks if needed later
    let tgt_nid = make_id(&[&rel_key]);
    if tgt_nid == file_nid || !linked.insert(tgt_nid.clone()) {
        return;
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

fn normalize_rel_path(p: &str) -> String {
    let normalized = p.replace('\\', "/");
    let mut out: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                out.pop();
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
        // Cargo.toml package name / dependencies
        if path.file_name().and_then(|n| n.to_str()) == Some("Cargo.toml") {
            for line in text.lines() {
                let t = line.trim();
                if let Some(rest) = t.strip_prefix("name")
                    && let Some(eq) = rest.find('=')
                {
                    let val = rest[eq + 1..].trim().trim_matches('"');
                    if !val.is_empty() {
                        let nid = make_id(&[val]);
                        nodes.push(Node {
                            id: nid.clone(),
                            label: val.to_string(),
                            file_type: FileType::Code,
                            source_file: path_str.clone(),
                            source_location: Some("L1".into()),
                            community: None,
                            origin_file: None,
                        });
                        edges.push(Edge {
                            source: file_nid.clone(),
                            target: nid,
                            relation: "contains".into(),
                            confidence: Confidence::Extracted,
                            source_file: path_str.clone(),
                            source_location: Some("L1".into()),
                            weight: Some(1.0),
                            context: None,
                        });
                    }
                }
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
