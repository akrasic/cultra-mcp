use crate::ast::types::Symbol;
use crate::mcp::types::{Scope, SymbolType};
use anyhow::Result;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Extract symbols from Rust source code
pub fn extract_rust_symbols(root_node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    // Extract function definitions
    symbols.extend(extract_rust_functions(root_node, content)?);

    // Extract struct definitions
    symbols.extend(extract_rust_structs(root_node, content)?);

    // Extract trait definitions
    symbols.extend(extract_rust_traits(root_node, content)?);

    // Extract impl blocks
    symbols.extend(extract_rust_impls(root_node, content)?);

    Ok(symbols)
}

/// Extract Rust function definitions
fn extract_rust_functions(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (function_item
        name: (identifier) @func.name
        parameters: (parameters) @func.params
        return_type: (_)? @func.return) @func.decl
    "#;

    let language = tree_sitter_rust::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut params = String::new();
        let mut return_type = String::new();
        let mut func_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "func.name" => {
                    func_name = capture.node.utf8_text(content)?.to_string();
                }
                "func.params" => {
                    params = capture.node.utf8_text(content)?.to_string();
                }
                "func.return" => {
                    return_type = capture.node.utf8_text(content)?.to_string();
                }
                "func.decl" => {
                    func_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = func_node {
            // Check visibility
            let scope = extract_rust_visibility(&node, content);

            let mut signature = format!("fn {}{}", func_name, params);
            if !return_type.is_empty() {
                signature.push_str(" -> ");
                signature.push_str(&return_type);
            }

            let calls = extract_rust_calls(&node, content, &language)?;

            symbols.push(Symbol {
                symbol_type: SymbolType::Function,
                name: func_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::from_str(&scope),
                signature,
                parent: None,
                receiver: None,
                calls,
                documentation: None,
                parameters: Vec::new(), // TODO: Parse parameters
                return_type: if return_type.is_empty() {
                    None
                } else {
                    Some(return_type)
                },
            });
        }
    }

    Ok(symbols)
}

/// Extract Rust struct definitions
fn extract_rust_structs(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (struct_item
        name: (type_identifier) @struct.name) @struct.decl
    "#;

    let language = tree_sitter_rust::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut struct_name = String::new();
        let mut struct_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "struct.name" => {
                    struct_name = capture.node.utf8_text(content)?.to_string();
                }
                "struct.decl" => {
                    struct_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = struct_node {
            let scope = extract_rust_visibility(&node, content);
            let signature = format!("struct {}", struct_name);

            symbols.push(Symbol {
                symbol_type: SymbolType::Struct,
                name: struct_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::from_str(&scope),
                signature,
                parent: None,
                receiver: None,
                calls: Vec::new(),
                documentation: None,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }

    Ok(symbols)
}

/// Extract Rust trait definitions
fn extract_rust_traits(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (trait_item
        name: (type_identifier) @trait.name) @trait.decl
    "#;

    let language = tree_sitter_rust::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut trait_name = String::new();
        let mut trait_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "trait.name" => {
                    trait_name = capture.node.utf8_text(content)?.to_string();
                }
                "trait.decl" => {
                    trait_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = trait_node {
            let scope = extract_rust_visibility(&node, content);
            let signature = format!("trait {}", trait_name);

            symbols.push(Symbol {
                symbol_type: SymbolType::Interface,
                name: trait_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::from_str(&scope),
                signature,
                parent: None,
                receiver: None,
                calls: Vec::new(),
                documentation: None,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }

    Ok(symbols)
}

/// Extract Rust impl blocks
fn extract_rust_impls(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (impl_item
        type: (type_identifier) @impl.type
        trait: (type_identifier)? @impl.trait) @impl.decl
    "#;

    let language = tree_sitter_rust::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut impl_type = String::new();
        let mut impl_trait = String::new();
        let mut impl_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "impl.type" => {
                    impl_type = capture.node.utf8_text(content)?.to_string();
                }
                "impl.trait" => {
                    impl_trait = capture.node.utf8_text(content)?.to_string();
                }
                "impl.decl" => {
                    impl_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = impl_node {
            let signature = if impl_trait.is_empty() {
                format!("impl {}", impl_type)
            } else {
                format!("impl {} for {}", impl_trait, impl_type)
            };

            symbols.push(Symbol {
                symbol_type: SymbolType::Type,
                name: impl_type.clone(),
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::Public,
                signature,
                parent: None,
                receiver: if impl_trait.is_empty() {
                    None
                } else {
                    Some(impl_trait)
                },
                calls: Vec::new(),
                documentation: None,
                parameters: Vec::new(),
                return_type: None,
            });
        }
    }

    Ok(symbols)
}

/// Extract visibility from Rust node
fn extract_rust_visibility(node: &tree_sitter::Node, content: &[u8]) -> String {
    // Check for pub, pub(crate), etc.
    for i in 0..node.child_count() {
        if let Some(child) = node.child(i as u32) {
            if child.kind() == "visibility_modifier" {
                let vis_text = child.utf8_text(content).unwrap_or("");
                if vis_text.contains("pub(crate)") {
                    return "pub(crate)".to_string();
                }
                if vis_text.contains("pub") {
                    return "pub".to_string();
                }
            }
        }
    }
    "private".to_string()
}

/// Extract function calls within Rust code
fn extract_rust_calls(
    func_node: &tree_sitter::Node,
    content: &[u8],
    language: &tree_sitter::Language,
) -> Result<Vec<String>> {
    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let query_source = r#"
    (call_expression
        function: [
            (identifier) @call.func
            (field_expression
                field: (field_identifier) @call.method)
        ])
    "#;

    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *func_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let call_name = capture.node.utf8_text(content)?.trim().to_string();
            if !call_name.is_empty() && !seen.contains(&call_name) {
                seen.insert(call_name.clone());
                calls.push(call_name);
            }
        }
    }

    Ok(calls)
}

/// Extract Rust imports (use statements)
pub fn extract_rust_imports(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<String>> {
    let mut imports = Vec::new();

    let query_source = r#"
    (use_declaration
        argument: (_) @import.path)
    "#;

    let language = tree_sitter_rust::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let import_path = capture.node.utf8_text(content)?.to_string();
            imports.push(import_path);
        }
    }

    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rust_symbol_extraction() {
        let source = r#"
use std::collections::HashMap;

/// A greeter struct
pub struct Greeter {
    name: String,
}

impl Greeter {
    pub fn new(name: String) -> Self {
        Self { name }
    }

    pub fn greet(&self) {
        println!("Hello, {}!", self.name);
    }
}

pub trait Greetable {
    fn greet(&self);
}

impl Greetable for Greeter {
    fn greet(&self) {
        println!("Greetable: {}", self.name);
    }
}

fn main() {
    let g = Greeter::new("World".to_string());
    g.greet();
}
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");

        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let symbols =
            extract_rust_symbols(&root_node, source.as_bytes()).expect("Failed to extract symbols");

        // Should extract struct, trait, 2 impls, and 1 function
        assert!(
            symbols.len() >= 4,
            "Should extract at least 4 symbols, got {}",
            symbols.len()
        );

        // Check struct
        let greeter_struct = symbols.iter().find(|s| s.name == "Greeter" && s.symbol_type == SymbolType::Struct);
        assert!(greeter_struct.is_some(), "Should find Greeter struct");
        let greeter_struct = greeter_struct.unwrap();
        assert_eq!(greeter_struct.scope, Scope::from_str("pub"));

        // Check trait
        let greetable_trait = symbols.iter().find(|s| s.name == "Greetable");
        assert!(greetable_trait.is_some(), "Should find Greetable trait");
        let greetable_trait = greetable_trait.unwrap();
        assert_eq!(greetable_trait.symbol_type, SymbolType::Interface);
        assert_eq!(greetable_trait.scope, Scope::from_str("pub"));

        // Check impl blocks
        let impl_blocks: Vec<_> = symbols.iter().filter(|s| s.symbol_type == SymbolType::Type).collect();
        assert_eq!(impl_blocks.len(), 2, "Should find 2 impl blocks");
    }

    #[test]
    fn test_rust_import_extraction() {
        let source = r#"
use std::collections::HashMap;
use std::fs;

fn main() {}
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_rust::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");
        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let imports =
            extract_rust_imports(&root_node, source.as_bytes()).expect("Failed to extract imports");

        assert_eq!(imports.len(), 2);
        assert!(imports.contains(&"std::collections::HashMap".to_string()));
        assert!(imports.contains(&"std::fs".to_string()));
    }
}
