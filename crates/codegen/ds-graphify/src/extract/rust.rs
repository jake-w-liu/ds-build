//! Rust AST extractor (Graphify-compatible).

use super::helpers::{Builder, node_text, walk_calls};
use super::ts_util;
use crate::ids::make_id;
use crate::schema::{Confidence, Extraction, FileType};
use std::path::Path;

pub fn extract_rust(path: &Path, source_key: &str) -> Extraction {
    let source = match std::fs::read(path) {
        Ok(s) => s,
        Err(e) => {
            return Extraction {
                error: Some(e.to_string()),
                ..Extraction::empty()
            };
        }
    };
    let lang = tree_sitter_rust::LANGUAGE.into();
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
            &["call_expression", "macro_invocation"],
            "function",
        );
    }

    // use declarations at top level
    extract_uses(tree.root_node(), &source, &mut b, &file_nid);

    b.finish()
}

fn walk<'a>(
    node: tree_sitter::Node<'a>,
    source: &[u8],
    b: &mut Builder,
    parent_impl: Option<&str>,
    file_nid: &str,
    function_bodies: &mut Vec<(String, tree_sitter::Node<'a>)>,
) {
    match node.kind() {
        "function_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let func_name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let (func_nid, label) = if let Some(impl_nid) = parent_impl {
                    (
                        make_id(&[impl_nid, &func_name]),
                        format!(".{func_name}()"),
                    )
                } else {
                    (
                        make_id(&[&b.stem, &func_name]),
                        format!("{func_name}()"),
                    )
                };
                b.add_node(&func_nid, &label, line, FileType::Code);
                if let Some(impl_nid) = parent_impl {
                    b.add_edge(impl_nid, &func_nid, "method", line, Confidence::Extracted);
                } else {
                    b.add_edge(file_nid, &func_nid, "contains", line, Confidence::Extracted);
                }
                emit_type_refs(node, source, b, &func_nid, line);
                if let Some(body) = node.child_by_field_name("body") {
                    function_bodies.push((func_nid, body));
                }
            }
            return;
        }
        "struct_item" | "enum_item" | "trait_item" | "type_item" | "union_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let item_name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let item_nid = make_id(&[&b.stem, &item_name]);
                b.add_node(&item_nid, &item_name, line, FileType::Code);
                b.add_edge(file_nid, &item_nid, "contains", line, Confidence::Extracted);

                // trait bounds / inheritance
                let mut c = node.walk();
                for child in node.children(&mut c) {
                    if child.kind() == "trait_bounds" || child.kind() == "type_parameters" {
                        collect_type_idents(child, source, b, &item_nid, line, "inherits");
                    }
                    if child.kind() == "field_declaration_list" {
                        collect_type_idents(child, source, b, &item_nid, line, "references");
                    }
                }
            }
            // still walk impl body methods via children for nested items? skip
            return;
        }
        "impl_item" => {
            let type_node = node.child_by_field_name("type");
            let line = node.start_position().row + 1;
            let type_name = type_node
                .map(|t| {
                    let text = node_text(t, source);
                    text.rsplit("::")
                        .next()
                        .unwrap_or(&text)
                        .split('<')
                        .next()
                        .unwrap_or(&text)
                        .trim()
                        .to_string()
                })
                .unwrap_or_else(|| "impl".into());
            let impl_nid = make_id(&[&b.stem, &type_name]);
            b.add_node(&impl_nid, &type_name, line, FileType::Code);
            b.add_edge(file_nid, &impl_nid, "contains", line, Confidence::Extracted);

            if let Some(trait_node) = node.child_by_field_name("trait") {
                let trait_text = node_text(trait_node, source);
                let trait_name = trait_text
                    .rsplit("::")
                    .next()
                    .unwrap_or(&trait_text)
                    .split('<')
                    .next()
                    .unwrap_or(&trait_text)
                    .trim();
                let tgt = b.ensure_named(trait_name, line);
                b.add_edge(&impl_nid, &tgt, "implements", line, Confidence::Extracted);
            }

            let mut c = node.walk();
            for child in node.children(&mut c) {
                if child.kind() == "declaration_list" {
                    let mut c2 = child.walk();
                    for member in child.children(&mut c2) {
                        walk(
                            member,
                            source,
                            b,
                            Some(&impl_nid),
                            file_nid,
                            function_bodies,
                        );
                    }
                }
            }
            return;
        }
        "mod_item" => {
            if let Some(name_node) = node.child_by_field_name("name") {
                let name = node_text(name_node, source);
                let line = node.start_position().row + 1;
                let nid = make_id(&[&b.stem, &name]);
                b.add_node(&nid, &name, line, FileType::Code);
                b.add_edge(file_nid, &nid, "contains", line, Confidence::Extracted);
            }
            // walk body
        }
        "const_item" | "static_item" | "macro_definition" => {
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
        walk(child, source, b, parent_impl, file_nid, function_bodies);
    }
}

fn extract_uses(node: tree_sitter::Node, source: &[u8], b: &mut Builder, file_nid: &str) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "use_declaration" {
            let line = n.start_position().row + 1;
            let text = node_text(n, source);
            // pull last path segment as import target label
            let cleaned = text
                .trim_start_matches("use")
                .trim()
                .trim_end_matches(';')
                .trim();
            let last = cleaned
                .rsplit([':', '{', ',', ' '])
                .find(|s| !s.is_empty() && *s != "self" && *s != "super" && *s != "crate")
                .unwrap_or(cleaned);
            let last = last.trim_matches(|c| c == '*' || c == '}');
            if !last.is_empty() {
                let tgt = b.ensure_named(last, line);
                b.add_edge(file_nid, &tgt, "imports", line, Confidence::Extracted);
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            // don't dive into function bodies for uses
            if !matches!(child.kind(), "function_item" | "impl_item") {
                stack.push(child);
            }
        }
    }
}

fn emit_type_refs(
    func_node: tree_sitter::Node,
    source: &[u8],
    b: &mut Builder,
    func_nid: &str,
    line: usize,
) {
    if let Some(params) = func_node.child_by_field_name("parameters") {
        collect_type_idents(params, source, b, func_nid, line, "references");
    }
    if let Some(ret) = func_node.child_by_field_name("return_type") {
        collect_type_idents(ret, source, b, func_nid, line, "references");
    }
}

fn collect_type_idents(
    node: tree_sitter::Node,
    source: &[u8],
    b: &mut Builder,
    from: &str,
    line: usize,
    relation: &str,
) {
    let mut stack = vec![node];
    while let Some(n) = stack.pop() {
        if n.kind() == "type_identifier" || n.kind() == "scoped_type_identifier" {
            let text = node_text(n, source);
            let name = text.rsplit("::").next().unwrap_or(&text);
            if !name.is_empty() && !is_primitive(name) {
                let tgt = b.ensure_named(name, line);
                if tgt != from {
                    b.add_edge(from, &tgt, relation, line, Confidence::Extracted);
                }
            }
        }
        let mut c = n.walk();
        for child in n.children(&mut c) {
            stack.push(child);
        }
    }
}

fn is_primitive(name: &str) -> bool {
    matches!(
        name,
        "u8" | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "i8"
            | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "str"
            | "String"
            | "Self"
            | "self"
    )
}
