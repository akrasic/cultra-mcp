use crate::ast::types::{Param, Symbol};
use crate::mcp::types::{Scope, SymbolType};
use anyhow::Result;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Extract symbols from TypeScript/JavaScript/TSX source code.
///
/// The `ts_lang` parameter must match the grammar the tree was parsed with —
/// `LANGUAGE_TYPESCRIPT` for .ts, `LANGUAGE_TSX` for .tsx, `LANGUAGE_JAVASCRIPT`
/// for .js/.jsx. Tree-sitter queries compiled against the wrong grammar silently
/// return zero matches, which is why TSX files previously returned 0 symbols.
pub fn extract_typescript_symbols(
    root_node: &tree_sitter::Node,
    content: &[u8],
    ts_lang: &tree_sitter::Language,
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    // Extract function declarations
    symbols.extend(extract_typescript_functions(root_node, content, ts_lang)?);

    // Extract class declarations
    symbols.extend(extract_typescript_classes(root_node, content, ts_lang)?);

    // Extract interface declarations
    symbols.extend(extract_typescript_interfaces(root_node, content, ts_lang)?);

    Ok(symbols)
}

/// Extract TypeScript function declarations (both regular and exported)
fn extract_typescript_functions(
    node: &tree_sitter::Node,
    content: &[u8],
    ts_lang: &tree_sitter::Language,
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    [
        (function_declaration
            name: (identifier) @func.name
            parameters: (formal_parameters) @func.params
            return_type: (type_annotation)? @func.return) @func.decl
        (export_statement
            declaration: (function_declaration
                name: (identifier) @func.name
                parameters: (formal_parameters) @func.params
                return_type: (type_annotation)? @func.return) @func.decl)
    ]
    "#;

    let language = ts_lang.clone();
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
            // Extract parameters with types
            let parameters = extract_typescript_parameters(&params);

            // Create concise signature
            let signature = if params.is_empty() || params == "()" {
                format!("{}()", func_name)
            } else {
                format!("{}(...)", func_name)
            };

            // Extract function calls
            let calls = extract_typescript_calls(&node, content, &language)?;

            // Extract return type (remove leading colon)
            let return_type_str = if !return_type.is_empty() {
                return_type.trim_start_matches(':').trim().to_string()
            } else {
                String::new()
            };

            symbols.push(Symbol {
                symbol_type: SymbolType::Function,
                name: func_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::Public,
                signature,
                parent: None,
                receiver: None,
                calls,
                documentation: None,
                parameters,
                return_type: if return_type_str.is_empty() {
                    None
                } else {
                    Some(return_type_str)
                },
            });
        }
    }

    Ok(symbols)
}

/// Extract TypeScript class declarations
fn extract_typescript_classes(
    node: &tree_sitter::Node,
    content: &[u8],
    ts_lang: &tree_sitter::Language,
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (class_declaration
        name: (type_identifier) @class.name) @class.decl
    "#;

    let language = ts_lang.clone();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut class_name = String::new();
        let mut class_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "class.name" => {
                    class_name = capture.node.utf8_text(content)?.to_string();
                }
                "class.decl" => {
                    class_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(cls_node) = class_node {
            let signature = format!("class {}", class_name);

            symbols.push(Symbol {
                symbol_type: SymbolType::Class,
                name: class_name.clone(),
                line: cls_node.start_position().row as u32 + 1,
                end_line: cls_node.end_position().row as u32 + 1,
                scope: Scope::Public,
                signature,
                parent: None,
                receiver: None,
                calls: Vec::new(),
                documentation: None,
                parameters: Vec::new(),
                return_type: None,
            });

            // CULTRA-967: extract class methods as individual symbols so
            // find_references / find_dead_code can anchor on them.
            symbols.extend(
                extract_typescript_class_methods(&cls_node, content, &class_name, ts_lang)?
            );
        }
    }

    Ok(symbols)
}

/// CULTRA-967: Extract methods from a class body as individual Symbol entries.
fn extract_typescript_class_methods(
    class_node: &tree_sitter::Node,
    content: &[u8],
    class_name: &str,
    language: &tree_sitter::Language,
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (method_definition
        name: (property_identifier) @method.name
        parameters: (formal_parameters) @method.params
        return_type: (type_annotation)? @method.return) @method.decl
    "#;

    let query = Query::new(language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *class_node, content);
    while let Some(match_) = matches.next() {
        let mut method_name = String::new();
        let mut params = String::new();
        let mut return_type = String::new();
        let mut method_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "method.name" => {
                    method_name = capture.node.utf8_text(content)?.to_string();
                }
                "method.params" => {
                    params = capture.node.utf8_text(content)?.to_string();
                }
                "method.return" => {
                    return_type = capture.node.utf8_text(content)?.to_string();
                }
                "method.decl" => {
                    method_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(m_node) = method_node {
            // Skip constructor — it's not a callable you'd reference externally
            if method_name == "constructor" {
                continue;
            }

            let parameters = extract_typescript_parameters(&params);

            let signature = if params.is_empty() || params == "()" {
                format!("{}()", method_name)
            } else {
                format!("{}(...)", method_name)
            };

            let calls = extract_typescript_calls(&m_node, content, language)?;

            let return_type_str = if !return_type.is_empty() {
                return_type.trim_start_matches(':').trim().to_string()
            } else {
                String::new()
            };

            // Determine scope from accessibility modifier. In tree-sitter's
            // TypeScript grammar, method_definition has an optional
            // accessibility_modifier child ("public"|"private"|"protected").
            let scope = {
                let mut found_scope = Scope::Public; // TS default is public
                let mut child = m_node.child(0);
                while let Some(c) = child {
                    if c.kind() == "accessibility_modifier" {
                        let modifier = c.utf8_text(content).unwrap_or("");
                        if modifier == "private" || modifier == "protected" {
                            found_scope = Scope::Private;
                        }
                        break;
                    }
                    // Stop after the first few children to avoid scanning body
                    if c.kind() == "statement_block" || c.kind() == "formal_parameters" {
                        break;
                    }
                    child = c.next_sibling();
                }
                found_scope
            };

            symbols.push(Symbol {
                symbol_type: SymbolType::Method,
                name: method_name,
                line: m_node.start_position().row as u32 + 1,
                end_line: m_node.end_position().row as u32 + 1,
                scope,
                signature,
                parent: Some(class_name.to_string()),
                receiver: None,
                calls,
                documentation: None,
                parameters,
                return_type: if return_type_str.is_empty() {
                    None
                } else {
                    Some(return_type_str)
                },
            });
        }
    }

    Ok(symbols)
}

/// Extract TypeScript interface declarations
fn extract_typescript_interfaces(
    node: &tree_sitter::Node,
    content: &[u8],
    ts_lang: &tree_sitter::Language,
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (interface_declaration
        name: (type_identifier) @interface.name) @interface.decl
    "#;

    let language = ts_lang.clone();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut interface_name = String::new();
        let mut interface_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "interface.name" => {
                    interface_name = capture.node.utf8_text(content)?.to_string();
                }
                "interface.decl" => {
                    interface_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = interface_node {
            let signature = format!("interface {}", interface_name);

            symbols.push(Symbol {
                symbol_type: SymbolType::Interface,
                name: interface_name,
                line: node.start_position().row as u32 + 1,
                end_line: node.end_position().row as u32 + 1,
                scope: Scope::Public,
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

/// Extract parameters from TypeScript parameter list string
fn extract_typescript_parameters(params_str: &str) -> Vec<Param> {
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

        // TypeScript parameters: "name: type" or "name" (no type annotation)
        if let Some(colon_idx) = part.find(':') {
            let param_name = part[..colon_idx].trim().to_string();
            let param_type = part[colon_idx + 1..].trim().to_string();

            params.push(Param {
                name: param_name,
                param_type,
            });
        } else {
            // No type annotation
            params.push(Param {
                name: part.to_string(),
                param_type: "any".to_string(),
            });
        }
    }

    params
}

/// Extract function calls within TypeScript code
fn extract_typescript_calls(
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
            (member_expression
                property: (property_identifier) @call.method)
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

/// Extract TypeScript imports
pub fn extract_typescript_imports(
    root_node: &tree_sitter::Node,
    content: &[u8],
    ts_lang: &tree_sitter::Language,
) -> Result<Vec<String>> {
    let mut imports = Vec::new();

    let query_source = r#"
    (import_statement
        source: (string) @import.source)
    "#;

    let language = ts_lang.clone();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let import_path = capture
                .node
                .utf8_text(content)?
                .trim_matches('"')
                .trim_matches('\'')
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
    fn test_typescript_symbol_extraction() {
        let source = r#"
import { User } from './types';

interface Config {
    port: number;
    host: string;
}

class Server {
    private config: Config;

    constructor(config: Config) {
        this.config = config;
    }

    start(): void {
        console.log("Starting server");
    }
}

function createServer(config: Config): Server {
    return new Server(config);
}

export function main() {
    const server = createServer({ port: 3000, host: "localhost" });
    server.start();
}
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");

        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let symbols = extract_typescript_symbols(&root_node, source.as_bytes(), &lang)
            .expect("Failed to extract symbols");

        // Should extract interface, class, and functions
        assert!(
            symbols.len() >= 3,
            "Should extract at least 3 symbols, got {}",
            symbols.len()
        );

        // Check interface
        let config_interface = symbols.iter().find(|s| s.name == "Config");
        assert!(config_interface.is_some(), "Should find Config interface");
        let config_interface = config_interface.unwrap();
        assert_eq!(config_interface.symbol_type, SymbolType::Interface);

        // Check class
        let server_class = symbols.iter().find(|s| s.name == "Server");
        assert!(server_class.is_some(), "Should find Server class");
        let server_class = server_class.unwrap();
        assert_eq!(server_class.symbol_type, SymbolType::Class);

        // Check function
        let create_server = symbols.iter().find(|s| s.name == "createServer");
        assert!(create_server.is_some(), "Should find createServer function");
        let create_server = create_server.unwrap();
        assert_eq!(create_server.symbol_type, SymbolType::Function);
        assert!(create_server.parameters.len() > 0, "Should have parameters");
    }

    #[test]
    fn test_typescript_import_extraction() {
        let source = r#"
import { User, Post } from './types';
import express from 'express';

function main() {}
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");
        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let imports = extract_typescript_imports(&root_node, source.as_bytes(), &lang)
            .expect("Failed to extract imports");

        assert_eq!(imports.len(), 2);
        assert!(imports.contains(&"./types".to_string()));
        assert!(imports.contains(&"express".to_string()));
    }

    #[test]
    fn test_typescript_parameter_extraction() {
        let params = "name: string, age: number, active";
        let result = extract_typescript_parameters(params);

        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "name");
        assert_eq!(result[0].param_type, "string");
        assert_eq!(result[1].name, "age");
        assert_eq!(result[1].param_type, "number");
        assert_eq!(result[2].name, "active");
        assert_eq!(result[2].param_type, "any");
    }
}
