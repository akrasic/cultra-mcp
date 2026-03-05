use crate::ast::types::{Param, Symbol};
use crate::mcp::types::{Scope, SymbolType};
use anyhow::Result;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Extract symbols from Python source code
pub fn extract_python_symbols(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    // Extract function definitions
    symbols.extend(extract_python_functions(root_node, content)?);

    // Extract class definitions
    symbols.extend(extract_python_classes(root_node, content)?);

    Ok(symbols)
}

/// Extract Python function definitions
fn extract_python_functions(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (function_definition
        name: (identifier) @func.name
        parameters: (parameters) @func.params) @func.decl
    "#;

    let language = tree_sitter_python::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut params = String::new();
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
                "func.decl" => {
                    func_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if let Some(node) = func_node {
            // Python scope: single underscore = private, double underscore (dunder) = public special method
            let scope = if func_name.starts_with("__") && func_name.ends_with("__") {
                "public"
            } else if func_name.starts_with('_') {
                "private"
            } else {
                "public"
            };

            // Extract parameters with type hints
            let parameters = extract_python_parameters(&params);

            let signature = format!("def {}{}", func_name, params);

            // Extract function calls
            let calls = extract_python_calls(&node, content, &language)?;

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
                return_type: None,
            });
        }
    }

    Ok(symbols)
}

/// Extract Python class definitions
fn extract_python_classes(
    node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Symbol>> {
    let mut symbols = Vec::new();

    let query_source = r#"
    (class_definition
        name: (identifier) @class.name) @class.decl
    "#;

    let language = tree_sitter_python::LANGUAGE.into();
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

        if let Some(node) = class_node {
            // Python scope: single underscore = private, dunder names are public
            let scope = if class_name.starts_with("__") && class_name.ends_with("__") {
                "public"
            } else if class_name.starts_with('_') {
                "private"
            } else {
                "public"
            };

            let signature = format!("class {}", class_name);

            symbols.push(Symbol {
                symbol_type: SymbolType::Class,
                name: class_name,
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

/// Extract parameters from Python parameter list string
fn extract_python_parameters(params_str: &str) -> Vec<Param> {
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

        // Skip self and cls
        if part.is_empty() || part == "self" || part == "cls" {
            continue;
        }

        // Python parameters: "name: type" or "name" (no type hint)
        // Handle default values: "name: type = value" or "name = value"
        if let Some(colon_idx) = part.find(':') {
            let param_name = part[..colon_idx].trim().to_string();
            let type_part = part[colon_idx + 1..].trim();

            // Remove default value if present
            let param_type = if let Some(eq_idx) = type_part.find('=') {
                type_part[..eq_idx].trim().to_string()
            } else {
                type_part.to_string()
            };

            params.push(Param {
                name: param_name,
                param_type,
            });
        } else {
            // No type hint - check for default value
            let param_name = if let Some(eq_idx) = part.find('=') {
                part[..eq_idx].trim().to_string()
            } else {
                part.to_string()
            };

            params.push(Param {
                name: param_name,
                param_type: "Any".to_string(),
            });
        }
    }

    params
}

/// Extract function calls within Python code
fn extract_python_calls(
    func_node: &tree_sitter::Node,
    content: &[u8],
    language: &tree_sitter::Language,
) -> Result<Vec<String>> {
    let mut calls = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let query_source = r#"
    (call
        function: [
            (identifier) @call.func
            (attribute
                attribute: (identifier) @call.method)
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

/// Extract Python imports
pub fn extract_python_imports(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<String>> {
    let mut imports = Vec::new();
    let mut seen = std::collections::HashSet::new();

    let language = tree_sitter_python::LANGUAGE.into();

    // Extract "import x" statements
    let import_query = r#"
    (import_statement
        name: (dotted_name) @import.name)
    "#;

    let query = Query::new(&language, import_query)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let import_name = capture.node.utf8_text(content)?.to_string();
            if !seen.contains(&import_name) {
                seen.insert(import_name.clone());
                imports.push(import_name);
            }
        }
    }

    // Extract "from x import y" statements
    let from_query = r#"
    (import_from_statement
        module_name: (dotted_name) @import.module)
    "#;

    let query = Query::new(&language, from_query)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let import_name = capture.node.utf8_text(content)?.to_string();
            if !seen.contains(&import_name) {
                seen.insert(import_name.clone());
                imports.push(import_name);
            }
        }
    }

    Ok(imports)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_python_symbol_extraction() {
        let source = r#"
import os
from typing import List

class Calculator:
    """A simple calculator class."""

    def __init__(self, name: str):
        self.name = name

    def add(self, x: int, y: int) -> int:
        result = self._calculate(x, y)
        return result

    def _calculate(self, a: int, b: int) -> int:
        return a + b

def create_calculator(name: str = "default") -> Calculator:
    return Calculator(name)

def _private_function():
    print("This is private")
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");

        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let symbols =
            extract_python_symbols(&root_node, source.as_bytes()).expect("Failed to extract symbols");

        // Should extract class and functions
        assert!(
            symbols.len() >= 3,
            "Should extract at least 3 symbols, got {}",
            symbols.len()
        );

        // Check class
        let calculator_class = symbols.iter().find(|s| s.name == "Calculator");
        assert!(calculator_class.is_some(), "Should find Calculator class");
        let calculator_class = calculator_class.unwrap();
        assert_eq!(calculator_class.symbol_type, SymbolType::Class);
        assert_eq!(calculator_class.scope, Scope::Public);

        // Check public function
        let create_func = symbols.iter().find(|s| s.name == "create_calculator");
        assert!(create_func.is_some(), "Should find create_calculator function");
        let create_func = create_func.unwrap();
        assert_eq!(create_func.symbol_type, SymbolType::Function);
        assert_eq!(create_func.scope, Scope::Public);

        // Check private function
        let private_func = symbols.iter().find(|s| s.name == "_private_function");
        assert!(private_func.is_some(), "Should find _private_function");
        let private_func = private_func.unwrap();
        assert_eq!(private_func.scope, Scope::Private);
    }

    #[test]
    fn test_python_import_extraction() {
        let source = r#"
import os
import sys
from typing import List, Dict

def main():
    pass
"#;

        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_python::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");
        let tree = parser.parse(source, None).expect("Failed to parse");
        let root_node = tree.root_node();

        let imports =
            extract_python_imports(&root_node, source.as_bytes()).expect("Failed to extract imports");

        assert_eq!(imports.len(), 3);
        assert!(imports.contains(&"os".to_string()));
        assert!(imports.contains(&"sys".to_string()));
        assert!(imports.contains(&"typing".to_string()));
    }

    #[test]
    fn test_python_parameter_extraction() {
        let params = "self, name: str, age: int = 18, active";
        let result = extract_python_parameters(params);

        // self should be skipped
        assert_eq!(result.len(), 3);
        assert_eq!(result[0].name, "name");
        assert_eq!(result[0].param_type, "str");
        assert_eq!(result[1].name, "age");
        assert_eq!(result[1].param_type, "int");
        assert_eq!(result[2].name, "active");
        assert_eq!(result[2].param_type, "Any");
    }
}
