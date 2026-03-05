use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Complete interface implementation analysis for Go
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceAnalysis {
    pub interfaces: Vec<InterfaceInfo>,
    pub implementations: Vec<ImplementationInfo>,
    pub file: String,
}

/// Interface definition with method signatures
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InterfaceInfo {
    pub name: String,
    pub methods: Vec<MethodSpec>,
    pub location: String,
}

/// Method specification in an interface
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MethodSpec {
    pub name: String,
    pub parameters: Vec<Param>,
    pub returns: String,
}

/// Parameter in a method signature
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    pub name: String,
    #[serde(rename = "type")]
    pub param_type: String,
}

/// Implementation information for a type
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImplementationInfo {
    pub type_name: String,
    pub implements: Vec<String>,        // Fully implemented interfaces
    pub partial_implements: Vec<String>, // Partially implemented interfaces
    pub methods: Vec<String>,            // Method names on this type
    pub missing_methods: HashMap<String, Vec<String>>, // Interface -> missing method names
    pub location: String,
}

/// Find Go interface implementations
pub fn find_interface_implementations(
    file_path: &str,
    interface_name: Option<&str>,
) -> Result<InterfaceAnalysis> {
    // Read file content
    let content = fs::read_to_string(file_path)?;
    let content_bytes = content.as_bytes();

    // Parse with tree-sitter
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_go::LANGUAGE.into(); parser.set_language(&lang)?;

    let tree = parser
        .parse(&content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Go file"))?;

    let root_node = tree.root_node();

    // Extract interfaces
    let mut interfaces = extract_interfaces(&root_node, content_bytes)?;

    // Filter by interface name if specified
    if let Some(name) = interface_name {
        interfaces.retain(|iface| iface.name == name);
    }

    // Extract implementations
    let implementations = extract_implementations(&root_node, content_bytes, &interfaces)?;

    Ok(InterfaceAnalysis {
        interfaces,
        implementations,
        file: file_path.to_string(),
    })
}

/// Extract all interface definitions
fn extract_interfaces(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<InterfaceInfo>> {
    let mut interfaces = Vec::new();

    let query_source = r#"
    (type_declaration
        (type_spec
            name: (type_identifier) @interface.name
            type: (interface_type) @interface.type)) @interface.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut interface_name = String::new();
        let mut interface_type_node: Option<tree_sitter::Node> = None;
        let mut interface_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "interface.name" => {
                    interface_name = capture.node.utf8_text(content)?.to_string();
                }
                "interface.type" => {
                    interface_type_node = Some(capture.node);
                }
                "interface.decl" => {
                    interface_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if !interface_name.is_empty() && interface_node.is_some() {
            let methods = if let Some(type_node) = interface_type_node {
                extract_interface_methods(&type_node, content)?
            } else {
                Vec::new()
            };

            interfaces.push(InterfaceInfo {
                name: interface_name,
                methods,
                location: format_location(&interface_node.unwrap()),
            });
        }
    }

    Ok(interfaces)
}

/// Extract method specifications from interface
fn extract_interface_methods(
    interface_type_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<MethodSpec>> {
    let mut methods = Vec::new();

    for i in 0..interface_type_node.child_count() {
        if let Some(child) = interface_type_node.child(i as u32) {
            if child.kind() != "method_elem" {
                continue;
            }

            let mut method_name = String::new();
            let mut params = Vec::new();
            let mut returns = String::new();

            for j in 0..child.child_count() {
                if let Some(elem_child) = child.child(j as u32) {
                    match elem_child.kind() {
                        "field_identifier" => {
                            method_name = elem_child.utf8_text(content)?.to_string();
                        }
                        "parameter_list" => {
                            // First parameter_list is params, second is returns
                            if params.is_empty() {
                                params = extract_parameters(&elem_child, content)?;
                            } else {
                                returns = elem_child.utf8_text(content)?.to_string();
                            }
                        }
                        "type_identifier" | "qualified_type" => {
                            // Single return type (no parens)
                            if returns.is_empty() {
                                returns = elem_child.utf8_text(content)?.to_string();
                            }
                        }
                        _ => {}
                    }
                }
            }

            if !method_name.is_empty() {
                methods.push(MethodSpec {
                    name: method_name,
                    parameters: params,
                    returns,
                });
            }
        }
    }

    Ok(methods)
}

/// Extract parameters from a parameter_list node
fn extract_parameters(
    param_list_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<Param>> {
    let mut params = Vec::new();

    for i in 0..param_list_node.child_count() {
        if let Some(child) = param_list_node.child(i as u32) {
            if child.kind() != "parameter_declaration" {
                continue;
            }

            let mut param_name = String::new();
            let mut param_type = String::new();

            for j in 0..child.child_count() {
                if let Some(param_child) = child.child(j as u32) {
                    match param_child.kind() {
                        "identifier" => {
                            param_name = param_child.utf8_text(content)?.to_string();
                        }
                        "type_identifier" | "pointer_type" | "slice_type"
                        | "array_type" | "qualified_type" => {
                            param_type = param_child.utf8_text(content)?.to_string();
                        }
                        _ => {}
                    }
                }
            }

            if !param_type.is_empty() {
                params.push(Param {
                    name: param_name,
                    param_type,
                });
            }
        }
    }

    Ok(params)
}

/// Extract implementations by matching type methods against interfaces
fn extract_implementations(
    root_node: &tree_sitter::Node,
    content: &[u8],
    interfaces: &[InterfaceInfo],
) -> Result<Vec<ImplementationInfo>> {
    let mut implementations = Vec::new();

    // Build map of type -> methods
    let (type_method_map, type_locations) = extract_type_methods(root_node, content)?;

    // Match types against interfaces
    for (type_name, methods) in &type_method_map {
        let mut impl_info = ImplementationInfo {
            type_name: type_name.clone(),
            methods: methods.clone(),
            implements: Vec::new(),
            partial_implements: Vec::new(),
            missing_methods: HashMap::new(),
            location: type_locations.get(type_name).cloned().unwrap_or_default(),
        };

        // Check each interface
        for iface in interfaces {
            let missing = find_missing_methods(methods, &iface.methods);

            if missing.is_empty() {
                // Fully implements
                impl_info.implements.push(iface.name.clone());
            } else if missing.len() < iface.methods.len() {
                // Partially implements (has some methods but not all)
                impl_info.partial_implements.push(iface.name.clone());
                impl_info.missing_methods.insert(iface.name.clone(), missing);
            }
        }

        implementations.push(impl_info);
    }

    Ok(implementations)
}

/// Extract all method declarations and their receivers
fn extract_type_methods(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<(HashMap<String, Vec<String>>, HashMap<String, String>)> {
    let mut type_method_map: HashMap<String, Vec<String>> = HashMap::new();
    let mut type_locations: HashMap<String, String> = HashMap::new();

    let query_source = r#"
    (method_declaration
        receiver: (parameter_list
            (parameter_declaration
                type: (_) @receiver.type))
        name: (field_identifier) @method.name) @method.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut receiver_type = String::new();
        let mut method_name = String::new();
        let mut method_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "receiver.type" => {
                    // Remove pointer prefix
                    let raw_type = capture.node.utf8_text(content)?.to_string();
                    receiver_type = raw_type.trim_start_matches('*').to_string();
                }
                "method.name" => {
                    method_name = capture.node.utf8_text(content)?.to_string();
                }
                "method.decl" => {
                    method_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if !receiver_type.is_empty() && !method_name.is_empty() {
            type_method_map
                .entry(receiver_type.clone())
                .or_insert_with(Vec::new)
                .push(method_name);

            if !type_locations.contains_key(&receiver_type) {
                if let Some(node) = method_node {
                    type_locations.insert(receiver_type, format_location(&node));
                }
            }
        }
    }

    Ok((type_method_map, type_locations))
}

/// Find missing methods from interface that are not in type methods
fn find_missing_methods(type_methods: &[String], interface_methods: &[MethodSpec]) -> Vec<String> {
    let mut missing = Vec::new();
    let method_set: std::collections::HashSet<_> = type_methods.iter().collect();

    for iface_method in interface_methods {
        if !method_set.contains(&iface_method.name) {
            missing.push(iface_method.name.clone());
        }
    }

    missing
}

fn format_location(node: &tree_sitter::Node) -> String {
    let start = node.start_position().row + 1;
    let end = node.end_position().row + 1;
    if start == end {
        format!("{}", start)
    } else {
        format!("{}-{}", start, end)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_interface_implementations() {
        let test_code = r#"package main

// Storage interface defines storage operations
type Storage interface {
	Get(key string) (string, error)
	Set(key string, value string) error
	Delete(key string) error
}

// Cache interface
type Cache interface {
	Get(key string) (string, error)
	Set(key string, value string) error
	Expire(key string, seconds int) error
}

// MemoryStorage fully implements Storage
type MemoryStorage struct {
	data map[string]string
}

func (m *MemoryStorage) Get(key string) (string, error) {
	return m.data[key], nil
}

func (m *MemoryStorage) Set(key string, value string) error {
	m.data[key] = value
	return nil
}

func (m *MemoryStorage) Delete(key string) error {
	delete(m.data, key)
	return nil
}

// PartialStorage partially implements Storage (missing Delete)
type PartialStorage struct{}

func (p *PartialStorage) Get(key string) (string, error) {
	return "", nil
}

func (p *PartialStorage) Set(key string, value string) error {
	return nil
}
"#;

        // Write to temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_interfaces.go");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(test_code.as_bytes())
            .expect("Failed to write temp file");

        // Analyze
        let result = find_interface_implementations(test_file.to_str().unwrap(), None);
        assert!(result.is_ok(), "Analysis should succeed");

        let analysis = result.unwrap();

        // Verify interfaces
        assert_eq!(analysis.interfaces.len(), 2, "Should find 2 interfaces");

        // Verify Storage interface
        let storage_interface = analysis
            .interfaces
            .iter()
            .find(|i| i.name == "Storage");
        assert!(storage_interface.is_some(), "Should find Storage interface");

        let storage = storage_interface.unwrap();
        assert_eq!(storage.methods.len(), 3, "Storage should have 3 methods");

        // Check methods
        let method_names: Vec<_> = storage.methods.iter().map(|m| m.name.as_str()).collect();
        assert!(method_names.contains(&"Get"), "Should have Get method");
        assert!(method_names.contains(&"Set"), "Should have Set method");
        assert!(method_names.contains(&"Delete"), "Should have Delete method");

        // Verify implementations
        let memory_storage = analysis
            .implementations
            .iter()
            .find(|i| i.type_name == "MemoryStorage");
        assert!(memory_storage.is_some(), "Should find MemoryStorage");

        let memory = memory_storage.unwrap();
        assert!(
            memory.implements.contains(&"Storage".to_string()),
            "MemoryStorage should fully implement Storage"
        );

        // Verify partial implementation
        let partial_storage = analysis
            .implementations
            .iter()
            .find(|i| i.type_name == "PartialStorage");
        assert!(partial_storage.is_some(), "Should find PartialStorage");

        let partial = partial_storage.unwrap();
        assert!(
            partial.partial_implements.contains(&"Storage".to_string()),
            "PartialStorage should partially implement Storage"
        );

        // Check missing methods
        let missing = partial.missing_methods.get("Storage");
        assert!(missing.is_some(), "Should have missing methods for Storage");

        let missing_methods = missing.unwrap();
        assert_eq!(missing_methods.len(), 1, "Should be missing 1 method");
        assert_eq!(missing_methods[0], "Delete", "Should be missing Delete method");

        // Clean up
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_interface_filter() {
        let test_code = r#"package main

type Reader interface {
	Read() string
}

type Writer interface {
	Write(s string)
}

type File struct{}

func (f *File) Read() string {
	return ""
}

func (f *File) Write(s string) {
}
"#;

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_filter.go");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(test_code.as_bytes())
            .expect("Failed to write temp file");

        // Analyze with filter
        let result = find_interface_implementations(test_file.to_str().unwrap(), Some("Reader"));
        assert!(result.is_ok());

        let analysis = result.unwrap();
        assert_eq!(analysis.interfaces.len(), 1, "Should find only Reader interface");
        assert_eq!(analysis.interfaces[0].name, "Reader");

        // File should fully implement Reader
        let file_impl = analysis
            .implementations
            .iter()
            .find(|i| i.type_name == "File");
        assert!(file_impl.is_some());
        assert!(file_impl.unwrap().implements.contains(&"Reader".to_string()));

        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_method_extraction() {
        let methods = vec!["Get".to_string(), "Set".to_string()];
        let interface_methods = vec![
            MethodSpec {
                name: "Get".to_string(),
                parameters: vec![],
                returns: "string".to_string(),
            },
            MethodSpec {
                name: "Set".to_string(),
                parameters: vec![],
                returns: "error".to_string(),
            },
            MethodSpec {
                name: "Delete".to_string(),
                parameters: vec![],
                returns: "error".to_string(),
            },
        ];

        let missing = find_missing_methods(&methods, &interface_methods);
        assert_eq!(missing.len(), 1);
        assert_eq!(missing[0], "Delete");
    }

    #[test]
    fn test_interface_ast_structure() {
        let code = r#"package main

type Storage interface {
	Get(key string) (string, error)
	Set(key string, value string) error
}
"#;
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE.into(); parser.set_language(&lang).unwrap();
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        fn print_node(node: &tree_sitter::Node, content: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(content).unwrap_or("");
            let preview = if text.len() > 60 {
                format!("{}...", &text[..60].replace('\n', " "))
            } else {
                text.replace('\n', " ")
            };
            eprintln!("{}{} [{}]", indent, node.kind(), preview);

            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    print_node(&child, content, depth + 1);
                }
            }
        }

        print_node(&root, code.as_bytes(), 0);
    }
}
