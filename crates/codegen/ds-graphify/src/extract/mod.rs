//! Structural extractors (tree-sitter AST + markdown).
//!
//! Produces Graphify-compatible `Extraction` dicts. Code extractors emit
//! EXTRACTED edges for contains/imports/inherits/method and INFERRED for
//! call-graph second pass.

mod go;
mod javascript;
mod markdown;
mod python;
mod rust;
mod ts_util;

use crate::schema::Extraction;
use rayon::prelude::*;
use std::path::{Path, PathBuf};

/// Extract a single file based on extension.
///
/// `source_key` is the portable path used for node IDs and `source_file`
/// (prefer root-relative with `/` separators). `path` is the on-disk path to read.
pub fn extract_file(path: &Path) -> Extraction {
    let key = path.display().to_string().replace('\\', "/");
    extract_file_keyed(path, &key)
}

/// Extract using an explicit portable `source_key` (usually root-relative).
pub fn extract_file_keyed(path: &Path, source_key: &str) -> Extraction {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "rs" => rust::extract_rust(path, source_key),
        "py" | "pyi" => python::extract_python(path, source_key),
        "go" => go::extract_go(path, source_key),
        "js" | "jsx" | "mjs" | "cjs" => javascript::extract_js(path, source_key, false),
        "ts" | "tsx" | "mts" | "cts" => javascript::extract_js(path, source_key, true),
        "md" | "mdx" | "qmd" | "txt" | "rst" => markdown::extract_markdown(path, source_key),
        "json" | "toml" | "yaml" | "yml" => markdown::extract_config_stub(path, source_key),
        _ => Extraction::empty(),
    }
}

/// Root-relative portable key for a path under `root`.
pub fn portable_source_key(path: &Path, root: &Path) -> String {
    path.strip_prefix(root)
        .map(|r| r.display().to_string())
        .unwrap_or_else(|_| path.display().to_string())
        .replace('\\', "/")
}

/// Extract many files in parallel and merge.
pub fn extract_many(paths: &[PathBuf], root: &Path) -> Extraction {
    let results: Vec<Extraction> = paths
        .par_iter()
        .map(|p| {
            let key = portable_source_key(p, root);
            extract_file_keyed(p, &key)
        })
        .collect();

    let mut merged = Extraction::empty();
    for r in results {
        merged.merge(r);
    }
    // Second-pass: resolve INFERRED calls across files by bare label match.
    resolve_cross_file_calls(&mut merged);
    merged
}

fn resolve_cross_file_calls(extraction: &mut Extraction) {
    use std::collections::HashMap;
    // label (without trailing ()) -> preferred definition id (has source_file non-empty)
    let mut defs: HashMap<String, String> = HashMap::new();
    for n in &extraction.nodes {
        if n.source_file.is_empty() {
            continue;
        }
        let bare = n.label.trim_end_matches("()").trim_start_matches('.').to_string();
        if bare.is_empty() {
            continue;
        }
        defs.entry(bare.to_ascii_lowercase())
            .or_insert_with(|| n.id.clone());
    }

    // For edges that point at sourceless stubs, rewire to real defs when unique.
    let mut id_remap: HashMap<String, String> = HashMap::new();
    for n in &extraction.nodes {
        if !n.source_file.is_empty() {
            continue;
        }
        let bare = n.label.trim_end_matches("()").trim_start_matches('.');
        if let Some(real) = defs.get(&bare.to_ascii_lowercase())
            && real != &n.id
        {
            id_remap.insert(n.id.clone(), real.clone());
        }
    }
    if id_remap.is_empty() {
        return;
    }
    for e in &mut extraction.edges {
        if let Some(r) = id_remap.get(&e.source) {
            e.source = r.clone();
        }
        if let Some(r) = id_remap.get(&e.target) {
            e.target = r.clone();
        }
    }
    // Drop sourceless stubs that were remapped.
    extraction
        .nodes
        .retain(|n| !id_remap.contains_key(&n.id));
}

/// Shared helpers for language extractors.
pub(crate) mod helpers {
    use crate::ids::make_id;
    use crate::schema::{Confidence, Edge, Extraction, FileType, Node};
    use std::collections::HashSet;
    use std::path::Path;

    pub struct Builder {
        pub nodes: Vec<Node>,
        pub edges: Vec<Edge>,
        pub seen: HashSet<String>,
        pub path_str: String,
        pub stem: String,
        pub file_nid: String,
    }

    impl Builder {
        /// `source_key` is the portable path for IDs / source_file (root-relative).
        /// `path` is only used for the display basename when `source_key` has no name.
        pub fn new(path: &Path, source_key: &str) -> Self {
            let path_str = source_key.replace('\\', "/");
            let stem = Path::new(&path_str)
                .file_stem()
                .and_then(|s| s.to_str())
                .or_else(|| path.file_stem().and_then(|s| s.to_str()))
                .unwrap_or("file")
                .to_string();
            let file_nid = make_id(&[&path_str]);
            let mut b = Self {
                nodes: Vec::new(),
                edges: Vec::new(),
                seen: HashSet::new(),
                path_str,
                stem,
                file_nid: file_nid.clone(),
            };
            let name = Path::new(source_key)
                .file_name()
                .and_then(|n| n.to_str())
                .or_else(|| path.file_name().and_then(|n| n.to_str()))
                .unwrap_or("file")
                .to_string();
            b.add_node(&file_nid, &name, 1, FileType::Code);
            b
        }

        pub fn add_node(&mut self, id: &str, label: &str, line: usize, file_type: FileType) {
            if self.seen.insert(id.to_string()) {
                self.nodes.push(Node {
                    id: id.to_string(),
                    label: label.to_string(),
                    file_type,
                    source_file: self.path_str.clone(),
                    source_location: Some(format!("L{line}")),
                    community: None,
                    origin_file: None,
                });
            }
        }

        pub fn add_sourceless(&mut self, id: &str, label: &str) {
            if self.seen.insert(id.to_string()) {
                self.nodes.push(Node {
                    id: id.to_string(),
                    label: label.to_string(),
                    file_type: FileType::Code,
                    source_file: String::new(),
                    source_location: None,
                    community: None,
                    origin_file: Some(self.path_str.clone()),
                });
            }
        }

        pub fn add_edge(
            &mut self,
            source: &str,
            target: &str,
            relation: &str,
            line: usize,
            confidence: Confidence,
        ) {
            // Dedup identical edges within a file (e.g. struct + impl both
            // emit file --contains--> Type).
            if self.edges.iter().any(|e| {
                e.source == source
                    && e.target == target
                    && e.relation == relation
                    && e.confidence == confidence
            }) {
                return;
            }
            self.edges.push(Edge {
                source: source.to_string(),
                target: target.to_string(),
                relation: relation.to_string(),
                confidence,
                source_file: self.path_str.clone(),
                source_location: Some(format!("L{line}")),
                weight: Some(1.0),
                context: None,
            });
        }

        pub fn ensure_named(&mut self, name: &str, line: usize) -> String {
            // Prefer the same file-scoped id as definition sites (file_nid + name),
            // not stem alone — stems collide across paths like a/lib.rs vs b/lib.rs.
            let local = make_id(&[&self.file_nid, name]);
            if self.seen.contains(&local) {
                return local;
            }
            let bare = make_id(&[name]);
            if !self.seen.contains(&bare) {
                self.add_sourceless(&bare, name);
            }
            let _ = line;
            bare
        }

        pub fn finish(self) -> Extraction {
            Extraction {
                nodes: self.nodes,
                edges: self.edges,
                error: None,
                input_tokens: 0,
                output_tokens: 0,
            }
        }
    }

    pub fn node_text(node: tree_sitter::Node, source: &[u8]) -> String {
        node.utf8_text(source).unwrap_or("").to_string()
    }

    pub fn walk_calls(
        body: tree_sitter::Node,
        source: &[u8],
        func_nid: &str,
        b: &mut Builder,
        call_kinds: &[&str],
        name_field: &str,
    ) {
        let mut stack = vec![body];
        while let Some(n) = stack.pop() {
            if call_kinds.contains(&n.kind()) {
                let name_node = n.child_by_field_name(name_field).or_else(|| {
                    // field_expression.field for method calls
                    n.child_by_field_name("function")
                        .and_then(|f| f.child_by_field_name("field"))
                        .or_else(|| n.child_by_field_name("function"))
                });
                if let Some(nn) = name_node {
                    let name = node_text(nn, source);
                    let name = name.rsplit(['.', ':']).next().unwrap_or(&name);
                    if !name.is_empty() && !is_noise_call(name) {
                        let line = n.start_position().row + 1;
                        let tgt = b.ensure_named(name, line);
                        if tgt != func_nid {
                            b.add_edge(func_nid, &tgt, "calls", line, Confidence::Inferred);
                        }
                    }
                }
            }
            let mut c = n.walk();
            for child in n.children(&mut c) {
                stack.push(child);
            }
        }
    }

    fn is_noise_call(name: &str) -> bool {
        matches!(
            name,
            "println" | "print" | "format" | "panic" | "assert" | "unwrap" | "expect" | "ok"
                | "err" | "clone" | "to_string" | "into" | "from" | "new" | "default" | "len"
                | "push" | "pop" | "insert" | "get" | "set" | "map" | "filter" | "collect"
                | "iter" | "next" | "Some" | "None" | "Ok" | "Err" | "true" | "false"
                | "console" | "log" | "require" | "append" | "make"
        )
    }
}
