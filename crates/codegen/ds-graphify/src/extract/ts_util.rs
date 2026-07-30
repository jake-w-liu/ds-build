//! Shared tree-sitter parse helpers.

use tree_sitter::{Language, Parser, Tree};

pub fn parse(language: Language, source: &[u8]) -> Result<Tree, String> {
    let mut parser = Parser::new();
    parser.set_language(&language).map_err(|e| e.to_string())?;
    parser
        .parse(source, None)
        .ok_or_else(|| "tree-sitter parse returned None".to_string())
}
