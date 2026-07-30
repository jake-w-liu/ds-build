//! Go AST extractor (Graphify-compatible).

use super::helpers::{Builder, node_text, walk_calls};
use super::ts_util;
use crate::ids::make_id;
use crate::schema::{Confidence, Extraction, FileType};
use std::path::Path;

pub fn extract_go(path: &Path, source_key: &str) -> Extraction {
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            return Extraction {
                error: Some(e.to_string()),
                ..Extraction::empty()
            };
        }
    };
    let lang = tree_sitter_go::LANGUAGE.into();
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
    let pkg_scope = Path::new(source_key)
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .map(|parent| parent.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|| "root".to_string());
    let file_nid = b.file_nid.clone();
    let mut function_bodies: Vec<(String, tree_sitter::Node)> = Vec::new();

    walk(
        tree.root_node(),
        &source,
        &mut b,
        &file_nid,
        &pkg_scope,
        &mut function_bodies,
    );
    for (func_nid, body) in function_bodies {
        walk_calls(
            body,
            &source,
            &func_nid,
            &mut b,
            &["call_expression"],
            "function",
        );
    }
    b.finish()
}

fn walk<'a>(
    node: tree_sitter::Node<'a>,
    source: &[u8],
    b: &mut Builder,
    file_nid: &str,
    pkg_scope: &str,
    function_bodies: &mut Vec<(String, tree_sitter::Node<'a>)>,
) {
    match node.kind() {
        "function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let nid = make_id(&[pkg_scope, &name]);
                b.add_node(&nid, &format!("{name}()"), line, FileType::Code);
                b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
                if let Some(body) = node.child_by_field_name("body") {
                    function_bodies.push((nid, body));
                }
            }
            return;
        }
        "method_declaration" => {
            let name = node
                .child_by_field_name("name")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            let recv = node
                .child_by_field_name("receiver")
                .map(|n| node_text(n, source))
                .unwrap_or_default();
            // rough type from receiver "(t *Type)"
            let type_name = recv
                .split_whitespace()
                .last()
                .unwrap_or("recv")
                .trim_start_matches('*')
                .trim_matches(|c| c == '(' || c == ')')
                .rsplit('.')
                .next()
                .unwrap_or("recv")
                .split('[')
                .next()
                .unwrap_or("recv")
                .to_string();
            let line = node.start_position().row + 1;
            let type_nid = make_id(&[pkg_scope, &type_name]);
            b.add_sourceless(&type_nid, &type_name);
            b.add_edge(file_nid, &type_nid, "contains", line, Confidence::Extracted);
            let method_nid = make_id(&[&type_nid, &name]);
            b.add_node(&method_nid, &format!(".{name}()"), line, FileType::Code);
            b.add_edge(
                &type_nid,
                &method_nid,
                "method",
                line,
                Confidence::Extracted,
            );
            if let Some(body) = node.child_by_field_name("body") {
                function_bodies.push((method_nid, body));
            }
            return;
        }
        "type_declaration" => {
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if child.kind() == "type_spec"
                    && let Some(name_node) = child.child_by_field_name("name")
                {
                    let name = node_text(name_node, source);
                    let line = child.start_position().row + 1;
                    let nid = make_id(&[pkg_scope, &name]);
                    b.add_node(&nid, &name, line, FileType::Code);
                    b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
                }
            }
            return;
        }
        "import_declaration" => {
            let line = node.start_position().row + 1;
            let text = node_text(node, source);
            for part in text.split('"').skip(1).step_by(2) {
                let name = part.rsplit('/').next().unwrap_or(part);
                if !name.is_empty() {
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
        walk(child, source, b, file_nid, pkg_scope, function_bodies);
    }
}
