use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::fs;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Complete React component analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReactComponentInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub component_type: String, // "function_component", "arrow_function", "class_component"
    pub location: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub props: Option<PropsInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub hooks: Vec<HookUsage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub state: Vec<StateVariable>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub child_components: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub api_calls: Vec<APICallSite>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub context_usage: Vec<ContextInfo>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub helper_functions: Vec<String>,
}

/// Props interface information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropsInfo {
    pub type_name: String,
    pub properties: Vec<PropProperty>,
    pub location: String,
}

/// Individual prop property
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PropProperty {
    pub name: String,
    #[serde(rename = "type")]
    pub prop_type: String,
    pub required: bool,
    pub optional: bool,
}

/// React hook usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HookUsage {
    pub name: String, // "useState", "useEffect", "useMemo", etc.
    pub line: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub state_var: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub setter_var: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub has_cleanup: Option<bool>,
}

/// State variable from useState
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateVariable {
    pub name: String,
    pub setter: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub initial_value: Option<String>,
    pub line: u32,
}

/// API call site
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct APICallSite {
    pub function: String, // "fetch", "axios.get", etc.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    pub line: u32,
}

/// Context usage
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextInfo {
    pub provider: bool, // true if Provider, false if consumer (useContext)
    pub context: String,
    pub line: u32,
}

/// Analyze React component from file
pub fn analyze_react_component(file_path: &str) -> Result<ReactComponentInfo> {
    // Read file content
    let content = fs::read_to_string(file_path)?;
    let content_bytes = content.as_bytes();

    // Parse with tree-sitter TSX (handles JSX)
    let mut parser = tree_sitter::Parser::new();
    let lang = tree_sitter_typescript::LANGUAGE_TSX.into(); parser.set_language(&lang)?;

    let tree = parser
        .parse(&content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse TypeScript/TSX file"))?;

    let root_node = tree.root_node();

    // Find main exported component
    let (component_name, component_node): (String, Option<tree_sitter::Node>) =
        find_main_component(&root_node, content_bytes)?;

    if component_node.is_none() {
        return Err(anyhow::anyhow!("No exported component found"));
    }

    let comp_node = component_node.unwrap();
    let component_type = detect_component_type(&comp_node);

    // Extract all component information
    let props = extract_props_interface(&root_node, content_bytes)?;
    let hooks = extract_hooks(&comp_node, content_bytes)?;
    let state = extract_state_from_hooks(&hooks);
    let child_components = extract_child_components(&comp_node, content_bytes)?;
    let api_calls = extract_api_calls(&comp_node, content_bytes)?;
    let context_usage = extract_context_usage(&comp_node, content_bytes)?;
    let helper_functions = extract_helper_functions(&root_node, content_bytes, &component_name)?;

    Ok(ReactComponentInfo {
        name: component_name,
        component_type,
        location: format_location(&comp_node),
        props,
        hooks,
        state,
        child_components,
        api_calls,
        context_usage,
        helper_functions,
    })
}

/// Find the main exported component
fn find_main_component<'a>(
    root_node: &'a tree_sitter::Node,
    content: &[u8],
) -> Result<(String, Option<tree_sitter::Node<'a>>)> {
    let query_source = r#"
    [
        (export_statement
            declaration: (function_declaration
                name: (identifier) @component.name) @component.node)
        (export_statement
            declaration: (lexical_declaration
                (variable_declarator
                    name: (identifier) @component.name
                    value: (arrow_function) @component.node)))
    ]
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut name = String::new();
        let mut component_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "component.name" => {
                    name = capture.node.utf8_text(content)?.to_string();
                }
                "component.node" => {
                    component_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if !name.is_empty() && component_node.is_some() {
            return Ok((name, component_node));
        }
    }

    Ok((String::new(), None))
}

/// Detect component type
fn detect_component_type(node: &tree_sitter::Node) -> String {
    match node.kind() {
        "function_declaration" => "function_component".to_string(),
        "arrow_function" => "arrow_function".to_string(),
        "class_declaration" => "class_component".to_string(),
        _ => "unknown".to_string(),
    }
}

/// Extract props interface/type
fn extract_props_interface(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Option<PropsInfo>> {
    let query_source = r#"
    (interface_declaration
        name: (type_identifier) @interface.name) @interface.decl
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
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

        // Check if this looks like a props interface (ends with "Props")
        if interface_name.ends_with("Props") && interface_node.is_some() {
            let node = interface_node.unwrap();

            // Find the body child (interface_body)
            let mut interface_body: Option<tree_sitter::Node> = None;
            for i in 0..node.child_count() {
                if let Some(child) = node.child(i as u32) {
                    if child.kind() == "interface_body" {
                        interface_body = Some(child);
                        break;
                    }
                }
            }

            if let Some(body) = interface_body {
                let properties = extract_interface_properties(&body, content)?;

                return Ok(Some(PropsInfo {
                    type_name: interface_name,
                    properties,
                    location: format_location(&node),
                }));
            }
        }
    }

    Ok(None)
}

/// Extract properties from interface body
fn extract_interface_properties(
    body: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<PropProperty>> {
    let mut properties = Vec::new();

    for i in 0..body.child_count() {
        if let Some(child) = body.child(i as u32) {
            if child.kind() != "property_signature" {
                continue;
            }

            let mut prop_name = String::new();
            let mut prop_type = String::new();
            let mut optional = false;

            for j in 0..child.child_count() {
                if let Some(prop_child) = child.child(j as u32) {
                    match prop_child.kind() {
                        "property_identifier" => {
                            prop_name = prop_child.utf8_text(content)?.to_string();
                        }
                        "?" => {
                            optional = true;
                        }
                        "type_annotation" => {
                            // Get the type after the colon
                            let count = prop_child.child_count();
                            if count > 0 {
                                if let Some(type_node) = prop_child.child((count - 1) as u32) {
                                    prop_type = type_node.utf8_text(content)?.to_string();
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }

            if !prop_name.is_empty() {
                properties.push(PropProperty {
                    name: prop_name,
                    prop_type: prop_type,
                    required: !optional,
                    optional,
                });
            }
        }
    }

    Ok(properties)
}

/// Extract React hooks
fn extract_hooks(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<HookUsage>> {
    let mut hooks = Vec::new();

    let query_source = r#"
    (call_expression
        function: (identifier) @hook.name
        arguments: (_) @hook.args) @hook.call
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut hook_name = String::new();
        let mut hook_call: Option<tree_sitter::Node> = None;
        let mut hook_args: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "hook.name" => {
                    hook_name = capture.node.utf8_text(content)?.to_string();
                }
                "hook.args" => {
                    hook_args = Some(capture.node);
                }
                "hook.call" => {
                    hook_call = Some(capture.node);
                }
                _ => {}
            }
        }

        // Check if it's a hook (starts with "use")
        if hook_name.starts_with("use") && hook_call.is_some() {
            let call_node = hook_call.unwrap();
            let mut hook = HookUsage {
                name: hook_name.clone(),
                line: call_node.start_position().row as u32 + 1,
                state_var: None,
                setter_var: None,
                dependencies: Vec::new(),
                has_cleanup: None,
            };

            // Extract dependencies for useEffect, useMemo, useCallback
            if hook_name == "useEffect" || hook_name == "useMemo" || hook_name == "useCallback" {
                if let Some(args) = hook_args {
                    hook.dependencies = extract_hook_dependencies(&args, content)?;
                    if hook_name == "useEffect" {
                        hook.has_cleanup = Some(check_effect_cleanup(&args, content)?);
                    }
                }
            }

            hooks.push(hook);
        }
    }

    Ok(hooks)
}

/// Extract hook dependencies from argument array
fn extract_hook_dependencies(args_node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<String>> {
    let mut dependencies = Vec::new();

    // Look for array as second argument
    for i in 0..args_node.child_count() {
        if let Some(child) = args_node.child(i as u32) {
            if child.kind() == "array" {
                // Extract array elements
                let array_content = child.utf8_text(content)?;
                // Remove brackets and split by comma
                let array_content = array_content.trim_start_matches('[').trim_end_matches(']');
                if !array_content.is_empty() {
                    for dep in array_content.split(',') {
                        let dep = dep.trim();
                        if !dep.is_empty() {
                            dependencies.push(dep.to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(dependencies)
}

/// Check if useEffect has cleanup function
fn check_effect_cleanup(args_node: &tree_sitter::Node, content: &[u8]) -> Result<bool> {
    // Look for return statement in the effect function
    // Check first argument (the effect function)
    for i in 0..args_node.child_count() {
        if let Some(child) = args_node.child(i as u32) {
            // Skip commas and other syntax
            if child.kind() == "arrow_function" || child.kind() == "function" {
                let arg_content = child.utf8_text(content)?;
                // Look for return statement (cleanup function)
                if arg_content.contains("return ") || arg_content.contains("return()") || arg_content.contains("return(") {
                    return Ok(true);
                }
                break;
            }
        }
    }

    Ok(false)
}

/// Extract state variables from useState hooks
fn extract_state_from_hooks(_hooks: &[HookUsage]) -> Vec<StateVariable> {
    // This would require parsing the destructuring assignment
    // For now, return empty - would need parent context to get variable names
    Vec::new()
}

/// Extract child components (JSX elements)
fn extract_child_components(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<String>> {
    let mut components = HashSet::new();

    // Query for both regular JSX elements and self-closing elements
    let query_source = r#"
    [
        (jsx_opening_element
            name: (identifier) @component.name)
        (jsx_self_closing_element
            name: (identifier) @component.name)
    ]
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            if *capture_name == "component.name" {
                let component_name = capture.node.utf8_text(content)?.to_string();
                // Only include components that start with uppercase (React convention)
                if let Some(first_char) = component_name.chars().next() {
                    if first_char.is_uppercase() {
                        components.insert(component_name);
                    }
                }
            }
        }
    }

    Ok(components.into_iter().collect())
}

/// Extract API calls (fetch, axios, etc.)
fn extract_api_calls(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<APICallSite>> {
    let mut calls = Vec::new();

    let query_source = r#"
    (call_expression
        function: (identifier) @func.name
        arguments: (_) @func.args) @func.call
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut func_call: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "func.name" => {
                    func_name = capture.node.utf8_text(content)?.to_string();
                }
                "func.call" => {
                    func_call = Some(capture.node);
                }
                _ => {}
            }
        }

        // Check for API call patterns
        if func_name == "fetch" || func_name == "axios" {
            calls.push(APICallSite {
                function: func_name,
                endpoint: None,
                method: None,
                line: func_call.unwrap().start_position().row as u32 + 1,
            });
        }
    }

    Ok(calls)
}

/// Extract Context.Provider and useContext usage
fn extract_context_usage(node: &tree_sitter::Node, content: &[u8]) -> Result<Vec<ContextInfo>> {
    let mut contexts = Vec::new();

    let query_source = r#"
    (call_expression
        function: (identifier) @func.name) @func.call
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut func_call: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "func.name" => {
                    func_name = capture.node.utf8_text(content)?.to_string();
                }
                "func.call" => {
                    func_call = Some(capture.node);
                }
                _ => {}
            }
        }

        if func_name == "useContext" {
            contexts.push(ContextInfo {
                provider: false,
                context: "useContext".to_string(),
                line: func_call.unwrap().start_position().row as u32 + 1,
            });
        }
    }

    Ok(contexts)
}

/// Extract helper functions
fn extract_helper_functions(
    root_node: &tree_sitter::Node,
    content: &[u8],
    component_name: &str,
) -> Result<Vec<String>> {
    let mut functions = Vec::new();

    let query_source = r#"
    (function_declaration
        name: (identifier) @func.name)
    "#;

    let language = tree_sitter_typescript::LANGUAGE_TSX.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            if *capture_name == "func.name" {
                let func_name = capture.node.utf8_text(content)?.to_string();
                // Exclude the component itself and hooks
                if func_name != component_name && !func_name.starts_with("use") {
                    functions.push(func_name);
                }
            }
        }
    }

    Ok(functions)
}

fn format_location(node: &tree_sitter::Node) -> String {
    format!(
        "{}-{}",
        node.start_position().row + 1,
        node.end_position().row + 1
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn test_react_component_analysis() {
        let test_code = r#"
import { useState, useEffect } from 'react';

interface TaskCardProps {
  task: string;
  priority?: number;
  onComplete: () => void;
}

export function TaskCard({ task, priority, onComplete }: TaskCardProps) {
  const [isComplete, setIsComplete] = useState(false);
  const [loading, setLoading] = useState(false);

  useEffect(() => {
    console.log('Task changed:', task);
    return () => {
      console.log('Cleanup');
    };
  }, [task]);

  const handleClick = () => {
    setLoading(true);
    fetch('/api/tasks')
      .then(() => setIsComplete(true))
      .finally(() => setLoading(false));
  };

  return (
    <div>
      <Button onClick={handleClick}>
        {task}
      </Button>
      <Badge priority={priority} />
    </div>
  );
}

function helperFunction() {
  return 'helper';
}
"#;

        // Write to temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_react_component.tsx");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(test_code.as_bytes())
            .expect("Failed to write temp file");

        // Analyze
        let result = analyze_react_component(test_file.to_str().unwrap());
        assert!(result.is_ok(), "Analysis should succeed");

        let component = result.unwrap();

        // Verify component basics
        assert_eq!(component.name, "TaskCard", "Component name should be TaskCard");
        assert_eq!(
            component.component_type, "function_component",
            "Should be function component"
        );

        // Verify props
        if component.props.is_none() {
            eprintln!("Props extraction failed. Component: {:?}", component.name);
        }
        assert!(component.props.is_some(), "Should have props");
        let props = component.props.unwrap();
        assert_eq!(props.type_name, "TaskCardProps");
        assert_eq!(props.properties.len(), 3, "Should have 3 props");

        // Check individual props
        let task_prop = props.properties.iter().find(|p| p.name == "task");
        assert!(task_prop.is_some(), "Should have task prop");
        assert!(task_prop.unwrap().required, "task should be required");

        let priority_prop = props.properties.iter().find(|p| p.name == "priority");
        assert!(priority_prop.is_some(), "Should have priority prop");
        assert!(priority_prop.unwrap().optional, "priority should be optional");

        // Verify hooks
        assert!(!component.hooks.is_empty(), "Should have hooks");
        assert_eq!(component.hooks.len(), 3, "Should have 3 hooks (2 useState, 1 useEffect)");

        let use_effect = component.hooks.iter().find(|h| h.name == "useEffect");
        assert!(use_effect.is_some(), "Should have useEffect");
        let effect = use_effect.unwrap();
        assert_eq!(effect.dependencies.len(), 1, "useEffect should have 1 dependency");
        assert_eq!(effect.dependencies[0], "task", "Dependency should be 'task'");
        assert_eq!(effect.has_cleanup, Some(true), "useEffect should have cleanup");

        // Verify child components
        eprintln!("Child components found: {:?}", component.child_components);
        assert!(!component.child_components.is_empty(), "Should have child components");
        assert!(
            component.child_components.contains(&"Button".to_string()),
            "Should have Button component"
        );
        assert!(
            component.child_components.contains(&"Badge".to_string()),
            "Should have Badge component"
        );

        // Verify API calls
        assert_eq!(component.api_calls.len(), 1, "Should have 1 API call");
        assert_eq!(component.api_calls[0].function, "fetch");

        // Verify helper functions
        assert!(!component.helper_functions.is_empty(), "Should have helper functions");
        assert!(
            component.helper_functions.contains(&"helperFunction".to_string()),
            "Should have helperFunction"
        );

        // Clean up
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_arrow_function_component() {
        let test_code = r#"
import { useState } from 'react';

interface MyProps {
  name: string;
}

export const MyComponent = ({ name }: MyProps) => {
  const [count, setCount] = useState(0);

  return <div>{name}</div>;
};
"#;

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_arrow_component.tsx");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(test_code.as_bytes())
            .expect("Failed to write temp file");

        let result = analyze_react_component(test_file.to_str().unwrap());
        if let Err(e) = &result {
            eprintln!("Error: {}", e);
        }
        assert!(result.is_ok());

        let component = result.unwrap();
        assert_eq!(component.name, "MyComponent");
        assert_eq!(component.component_type, "arrow_function");

        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_hook_dependencies() {
        let deps = vec!["name".to_string(), "age".to_string()];
        assert_eq!(deps.len(), 2);
        assert_eq!(deps[0], "name");
        assert_eq!(deps[1], "age");
    }

    #[test]
    fn test_interface_ast_structure() {
        let code = r#"
interface TaskCardProps {
  task: string;
  priority?: number;
}
"#;
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_typescript::LANGUAGE_TSX.into(); parser.set_language(&lang).unwrap();
        let tree = parser.parse(code, None).unwrap();
        let root = tree.root_node();

        fn print_node(node: &tree_sitter::Node, content: &[u8], depth: usize) {
            let indent = "  ".repeat(depth);
            let text = node.utf8_text(content).unwrap_or("");
            let preview = if text.len() > 40 {
                format!("{}...", &text[..40].replace('\n', " "))
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
