//! JavaScript / TypeScript AST extractor (Graphify-compatible).

use super::helpers::{Builder, node_text, walk_calls};
use super::ts_util;
use crate::ids::make_id;
use crate::schema::{Confidence, Extraction, FileType};
use std::path::Path;

pub fn extract_js(path: &Path, source_key: &str, typescript: bool) -> Extraction {
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            return Extraction {
                error: Some(e.to_string()),
                ..Extraction::empty()
            };
        }
    };
    let lang = if typescript {
        if path
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| e == "tsx")
        {
            tree_sitter_typescript::LANGUAGE_TSX.into()
        } else {
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()
        }
    } else {
        tree_sitter_javascript::LANGUAGE.into()
    };
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
    parent_class: Option<&str>,
    file_nid: &str,
    function_bodies: &mut Vec<(String, tree_sitter::Node<'a>)>,
) {
    match node.kind() {
        "function_declaration" | "generator_function_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let nid = make_id(&[&b.stem, &name]);
                b.add_node(&nid, &format!("{name}()"), line, FileType::Code);
                b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
                if let Some(body) = node.child_by_field_name("body") {
                    function_bodies.push((nid, body));
                }
            }
            return;
        }
        "class_declaration" | "abstract_class_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let nid = make_id(&[&b.stem, &name]);
                b.add_node(&nid, &name, line, FileType::Code);
                b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
                // heritage
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "class_heritage" {
                        let t = node_text(child, source);
                        for tok in t.split(|c: char| !c.is_alphanumeric() && c != '_') {
                            if tok.is_empty()
                                || matches!(tok, "extends" | "implements")
                            {
                                continue;
                            }
                            let tgt = b.ensure_named(tok, line);
                            b.add_edge(&nid, &tgt, "inherits", line, Confidence::Extracted);
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
                            Some(&nid),
                            file_nid,
                            function_bodies,
                        );
                    }
                }
            }
            return;
        }
        "method_definition" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                if let Some(cls) = parent_class {
                    let nid = make_id(&[cls, &name]);
                    b.add_node(&nid, &format!(".{name}()"), line, FileType::Code);
                    b.add_edge(cls, &nid, "method", line, Confidence::Extracted);
                    if let Some(body) = node.child_by_field_name("body") {
                        function_bodies.push((nid, body));
                    }
                }
            }
            return;
        }
        "lexical_declaration" | "variable_declaration" => {
            // const foo = () => {}
            let mut c = node.walk();
            for child in node.children(&mut c) {
                if child.kind() == "variable_declarator" {
                    let name = child
                        .child_by_field_name("name")
                        .map(|n| node_text(n, source))
                        .unwrap_or_default();
                    let value = child.child_by_field_name("value");
                    if let Some(v) = value
                        && matches!(
                            v.kind(),
                            "arrow_function" | "function" | "function_expression"
                        )
                        && !name.is_empty()
                    {
                        let line = child.start_position().row + 1;
                        let nid = make_id(&[&b.stem, &name]);
                        b.add_node(&nid, &format!("{name}()"), line, FileType::Code);
                        b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
                        if let Some(body) = v.child_by_field_name("body") {
                            function_bodies.push((nid, body));
                        }
                    }
                }
            }
            return;
        }
        "import_statement" => {
            let line = node.start_position().row + 1;
            let text = node_text(node, source);
            // from 'module' or require-like
            for part in text.split(['\'', '"']).skip(1).step_by(2) {
                let name = part.rsplit(['/', '.']).next().unwrap_or(part);
                if !name.is_empty() && name != "js" && name != "ts" && name != "tsx" {
                    let tgt = b.ensure_named(name, line);
                    b.add_edge(file_nid, &tgt, "imports", line, Confidence::Extracted);
                }
            }
            return;
        }
        "interface_declaration" | "type_alias_declaration" | "enum_declaration" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let nid = make_id(&[&b.stem, &name]);
                b.add_node(&nid, &name, line, FileType::Code);
                b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
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
