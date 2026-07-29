//! Python AST extractor (Graphify-compatible).

use super::helpers::{Builder, node_text, walk_calls};
use super::ts_util;
use crate::ids::make_id;
use crate::schema::{Confidence, Extraction, FileType};
use std::path::Path;

pub fn extract_python(path: &Path, source_key: &str) -> Extraction {
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            return Extraction {
                error: Some(e.to_string()),
                ..Extraction::empty()
            };
        }
    };
    let lang = tree_sitter_python::LANGUAGE.into();
    let tree = match ts_util::parse(lang, &source) {
        Ok(t) => t,
        Err(e) => {
            return Extraction {
                error: Some(e),
                ..Extraction::empty()
            };
        }
    };

    let mut b = Builder::new(path, source_key);
    let file_nid = b.file_nid.clone();
    let mut function_bodies: Vec<(String, tree_sitter::Node)> = Vec::new();
    walk(
        tree.root_node(),
        &source,
        &mut b,
        None,
        &file_nid,
        &mut function_bodies,
    );
    for (func_nid, body) in function_bodies {
        walk_calls(
            body,
            &source,
            &func_nid,
            &mut b,
            &["call"],
            "function",
        );
    }
    b.finish()
}

fn walk<'a>(
    node: tree_sitter::Node<'a>,
    source: &[u8],
    b: &mut Builder,
    parent_class: Option<&str>,
    file_nid: &str,
    function_bodies: &mut Vec<(String, tree_sitter::Node<'a>)>,
) {
    match node.kind() {
        "function_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let func_name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let (func_nid, label) = if let Some(cls) = parent_class {
                    (make_id(&[cls, &func_name]), format!(".{func_name}()"))
                } else {
                    (make_id(&[&b.stem, &func_name]), format!("{func_name}()"))
                };
                b.add_node(&func_nid, &label, line, FileType::Code);
                if let Some(cls) = parent_class {
                    b.add_edge(cls, &func_nid, "method", line, Confidence::Extracted);
                } else {
                    b.add_edge(file_nid, &func_nid, "contains", line, Confidence::Extracted);
                }
                if let Some(body) = node.child_by_field_name("body") {
                    function_bodies.push((func_nid, body));
                }
            }
            return;
        }
        "class_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let cls_name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let cls_nid = make_id(&[&b.stem, &cls_name]);
                b.add_node(&cls_nid, &cls_name, line, FileType::Code);
                b.add_edge(file_nid, &cls_nid, "contains", line, Confidence::Extracted);
                if let Some(superclasses) = node.child_by_field_name("superclasses") {
                    let mut c = superclasses.walk();
                    for child in superclasses.children(&mut c) {
                        if child.kind() == "identifier" || child.kind() == "attribute" {
                            let t = node_text(child, source);
                            let name = t.rsplit('.').next().unwrap_or(&t);
                            let tgt = b.ensure_named(name, line);
                            b.add_edge(&cls_nid, &tgt, "inherits", line, Confidence::Extracted);
                        }
                    }
                }
                if let Some(body) = node.child_by_field_name("body") {
                    let mut c = body.walk();
                    for child in body.children(&mut c) {
                        walk(
                            child,
                            source,
                            b,
                            Some(&cls_nid),
                            file_nid,
                            function_bodies,
                        );
                    }
                }
            }
            return;
        }
        "import_statement" | "import_from_statement" => {
            let line = node.start_position().row + 1;
            let text = node_text(node, source);
            // crude: last identifier-ish token
            for part in text.split_whitespace() {
                let part = part.trim_matches(|c: char| !c.is_alphanumeric() && c != '_');
                if part.is_empty()
                    || matches!(
                        part,
                        "import" | "from" | "as" | "and" | "or" | "(" | ")" | ","
                    )
                {
                    continue;
                }
                let name = part.rsplit('.').next().unwrap_or(part);
                if name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    let tgt = b.ensure_named(name, line);
                    b.add_edge(file_nid, &tgt, "imports", line, Confidence::Extracted);
                }
            }
            return;
        }
        _ => {}
    }
    let mut c = node.walk();
    for child in node.children(&mut c) {
        walk(child, source, b, parent_class, file_nid, function_bodies);
    }
}
