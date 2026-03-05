use crate::ast::types::{Param, Symbol};
use crate::mcp::types::{Scope, SymbolType};
use anyhow::Result;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Extract symbols from Go source code
pub fn extract_go_symbols(root_node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    // Extract function declarations
    symbols.extend(extract_go_functions(root_node, content)?);

    // Extract method declarations
    symbols.extend(extract_go_methods(root_node, content)?);

    // Extract type declarations
    symbols.extend(extract_go_types(root_node, content)?);

    Ok(symbols)
}

/// Extract Go function declarations
fn extract_go_functions(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (function_declaration
        name: (identifier) @func.name
        parameters: (parameter_list) @func.params
        result: (_)? @func.result) @func.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut params = String::new();
        let mut result = String::new();
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
                "func.result" => {
                    result = capture.node.utf8_text(content)?.to_string();
                }
                "func.decl" => {
                    func_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = func_node {
            let scope = if func_name.chars().next().map_or(false, |c| c.is_uppercase()) {
                "exported"
            } else {
                "unexported"
            };

            let signature = if params.is_empty() || params == "()" {
                format!("{}()", func_name)
            } else {
                format!("{}(...)", func_name)
            };

            let parameters = extract_go_parameters(&params);
            let calls = extract_function_calls(&node, content, &language)?;
            let return_type = result.trim().to_string();

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
                parameters,
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

/// Extract Go method declarations
fn extract_go_methods(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (method_declaration
        receiver: (parameter_list
            (parameter_declaration
                name: (identifier)? @receiver.name
                type: (_) @receiver.type))
        name: (field_identifier) @method.name
        parameters: (parameter_list) @method.params
        result: (_)? @method.result) @method.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut method_name = String::new();
        let mut receiver_type = String::new();
        let mut params = String::new();
        let mut result = String::new();
        let mut method_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "method.name" => {
                    method_name = capture.node.utf8_text(content)?.to_string();
                }
                "receiver.type" => {
                    receiver_type = capture.node.utf8_text(content)?.to_string();
                }
                "method.params" => {
                    params = capture.node.utf8_text(content)?.to_string();
                }
                "method.result" => {
                    result = capture.node.utf8_text(content)?.to_string();
                }
                "method.decl" => {
                    method_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = method_node {
            let scope = if method_name
                .chars()
                .next()
                .map_or(false, |c| c.is_uppercase())
            {
                "exported"
            } else {
                "unexported"
            };

            let signature = if params.is_empty() || params == "()" {
                format!("{}()", method_name)
            } else {
                format!("{}(...)", method_name)
            };

            let parameters = extract_go_parameters(&params);
            let calls = extract_function_calls(&node, content, &language)?;
            let return_type = result.trim().to_string();

            symbols.push(Symbol {
                symbol_type: SymbolType::Method,
                name: method_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::from_str(&scope),
                signature,
                parent: None,
                receiver: Some(receiver_type),
                calls,
                documentation: None,
                parameters,
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

/// Extract Go type declarations
fn extract_go_types(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (type_declaration
        (type_spec
            name: (type_identifier) @type.name
            type: (_) @type.def)) @type.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut type_name = String::new();
        let mut type_def = String::new();
        let mut type_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "type.name" => {
                    type_name = capture.node.utf8_text(content)?.to_string();
                }
                "type.def" => {
                    let full_def = capture.node.utf8_text(content)?;
                    // Get first line only for brevity
                    type_def = full_def.lines().next().unwrap_or("").to_string();
                    if type_def.len() > 100 {
                        type_def.truncate(100);
                        type_def.push_str("...");
                    }
                }
                "type.decl" => {
                    type_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = type_node {
            let scope = if type_name
                .chars()
                .next()
                .map_or(false, |c| c.is_uppercase())
            {
                "exported"
            } else {
                "unexported"
            };

            let signature = format!("type {} {}", type_name, type_def);

            symbols.push(Symbol {
                symbol_type: SymbolType::Type,
                name: type_name,
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

/// Extract parameters from Go parameter list string
fn extract_go_parameters(params_str: &str) -> Vec<Param> {
    if params_str.is_empty() || params_str == "()" {
        return Vec::new();
    }

    let mut params = Vec::new();

    // Remove surrounding parentheses
    let params_str = params_str
        .trim_start_matches('(')
        .trim_end_matches(')')
        .trim();

    if params_str.is_empty() {
        return params;
    }

    // Split by comma (simple parsing)
    for part in params_str.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }

        // Go parameters: "name type" or just "type"
        let tokens: Vec<&str> = part.split_whitespace().collect();

        if tokens.len() == 1 {
            // Unnamed parameter (just type)
            params.push(Param {
                name: String::new(),
                param_type: tokens[0].to_string(),
            });
        } else if tokens.len() >= 2 {
            // Named parameter
            let param_type = tokens[tokens.len() - 1].to_string();
            let param_name = tokens[..tokens.len() - 1].join(" ");

            params.push(Param {
                name: param_name,
                param_type,
            });
        }
    }

    params
}

/// Extract function calls within a function body
fn extract_function_calls(
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
            (selector_expression
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

/// Extract Go imports
pub fn extract_go_imports(root_node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<String>> {
    let mut imports = Vec::new();

    // Query handles both single imports and import groups
    let query_source = r#"
    (import_declaration
        [
            (import_spec path: (interpreted_string_literal) @import.path)
            (import_spec_list (import_spec path: (interpreted_string_literal) @import.path))
        ])
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let import_path = capture
                .node
                .utf8_text(content)?
                .trim_matches('"')
                .to_string();
            imports.push(import_path);
        }
    }

    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_go_symbol_extraction() {
        let source = r#"
package main

import "fmt"

// HelloWorld prints a greeting
func HelloWorld() {
    fmt.Println("Hello")
}

// Greeter handles greetings
type Greeter struct {
    name string
}

// Greet returns a greeting
func (g *Greeter) Greet(message string) string {
    return fmt.Sprintf("%s: %s", g.name, message)
}
"#;

        // Parse with tree-sitter
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");

        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        // Extract symbols
        let symbols = extract_go_symbols(&root_node, source.as_bytes()).expect("Failed to extract symbols");

        // Should have extracted function, type, and method
        assert_eq!(symbols.len(), 3, "Should extract 3 symbols (function, type, method)");

        // Check function
        let hello_func = symbols.iter().find(|s| s.name == "HelloWorld");
        assert!(hello_func.is_some(), "Should find HelloWorld function");
        let hello_func = hello_func.unwrap();
        assert_eq!(hello_func.symbol_type, SymbolType::Function);
        assert_eq!(hello_func.scope, Scope::Exported);

        // Check type
        let greeter_type = symbols.iter().find(|s| s.name == "Greeter");
        assert!(greeter_type.is_some(), "Should find Greeter type");
        let greeter_type = greeter_type.unwrap();
        assert_eq!(greeter_type.symbol_type, SymbolType::Type);
        assert_eq!(greeter_type.scope, Scope::Exported);

        // Check method
        let greet_method = symbols.iter().find(|s| s.name == "Greet");
        assert!(greet_method.is_some(), "Should find Greet method");
        let greet_method = greet_method.unwrap();
        assert_eq!(greet_method.symbol_type, SymbolType::Method);
        assert_eq!(greet_method.scope, Scope::Exported);
        assert_eq!(greet_method.receiver, Some("*Greeter".to_string()));
    }

    #[test]
    fn test_go_import_extraction() {
        let source = r#"
package main

import (
    "fmt"
    "context"
)
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE.into(); parser.set_language(&lang).expect("Failed to set language");
        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let imports = extract_go_imports(&root_node, source.as_bytes()).expect("Failed to extract imports");

        assert_eq!(imports.len(), 2);
        assert!(imports.contains(&"fmt".to_string()));
        assert!(imports.contains(&"context".to_string()));
    }
}
