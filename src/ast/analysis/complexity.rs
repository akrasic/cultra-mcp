use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;

use crate::ast::util::detect_language;

/// Complexity analysis result for a file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexityAnalysis {
    pub file_path: String,
    pub language: String,
    pub functions: Vec<FunctionComplexity>,
    pub summary: ComplexitySummary,
}

/// Complexity metrics for a single function
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FunctionComplexity {
    pub name: String,
    pub location: String,
    pub line_start: u32,
    pub line_end: u32,
    pub lines: u32,
    pub cyclomatic: u32,
    pub cognitive: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>,
    pub rating: String, // "simple", "moderate", "complex", "very_complex"
}

/// Summary statistics for the file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComplexitySummary {
    pub total_functions: usize,
    pub avg_cyclomatic: f64,
    pub max_cyclomatic: u32,
    pub avg_cognitive: f64,
    pub max_cognitive: u32,
    pub complex_functions: usize,   // cyclomatic > 10
    pub very_complex_functions: usize, // cyclomatic > 20
    pub total_lines: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hotspot: Option<String>, // most complex function name
}

/// Analyze complexity of all functions in a file
pub fn analyze_complexity(file_path: &str) -> Result<ComplexityAnalysis> {
    let content = fs::read_to_string(file_path)?;
    let language = detect_language(file_path)
        .unwrap_or("unknown")
        .to_string();

    let mut parser = tree_sitter::Parser::new();

    // Svelte: extract script block, analyze as TypeScript with line offset
    let (effective_content, effective_language, line_offset) = if language == "svelte" {
        let (script, offset) = crate::ast::parser::extract_svelte_script(&content);
        if script.is_empty() {
            return Ok(ComplexityAnalysis {
                file_path: file_path.to_string(),
                language: "svelte".to_string(),
                functions: Vec::new(),
                summary: empty_summary(),
            });
        }
        (script, "typescript".to_string(), offset)
    } else {
        (content.clone(), language.clone(), 0)
    };
    let content_bytes = effective_content.as_bytes();

    let lang = match effective_language.as_str() {
        "go" => tree_sitter_go::LANGUAGE.into(),
        "typescript" => tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into(),
        "tsx" => tree_sitter_typescript::LANGUAGE_TSX.into(),
        "javascript" => tree_sitter_javascript::LANGUAGE.into(),
        "python" => tree_sitter_python::LANGUAGE.into(),
        "rust" => tree_sitter_rust::LANGUAGE.into(),
        "terraform" => tree_sitter_hcl::LANGUAGE.into(),
        _ => return Ok(ComplexityAnalysis {
            file_path: file_path.to_string(),
            language,
            functions: Vec::new(),
            summary: empty_summary(),
        }),
    };

    parser.set_language(&lang)?;
    let tree = match parser.parse(&effective_content, None) {
        Some(t) => t,
        None => return Ok(ComplexityAnalysis {
            file_path: file_path.to_string(),
            language,
            functions: Vec::new(),
            summary: empty_summary(),
        }),
    };

    let root = tree.root_node();
    let mut functions = extract_functions_with_complexity(&root, content_bytes, file_path, &effective_language);

    // Svelte: offset line numbers to match the original file
    if line_offset > 0 {
        for f in &mut functions {
            f.line_start += line_offset;
            f.line_end += line_offset;
            f.location = format!("{}:{}-{}", file_path, f.line_start, f.line_end);
        }
    }

    let summary = build_complexity_summary(&functions);

    Ok(ComplexityAnalysis {
        file_path: file_path.to_string(),
        language,
        functions,
        summary,
    })
}

fn empty_summary() -> ComplexitySummary {
    ComplexitySummary {
        total_functions: 0,
        avg_cyclomatic: 0.0,
        max_cyclomatic: 0,
        avg_cognitive: 0.0,
        max_cognitive: 0,
        complex_functions: 0,
        very_complex_functions: 0,
        total_lines: 0,
        hotspot: None,
    }
}

fn build_complexity_summary(functions: &[FunctionComplexity]) -> ComplexitySummary {
    if functions.is_empty() {
        return empty_summary();
    }

    let total = functions.len();
    let sum_cyc: u32 = functions.iter().map(|f| f.cyclomatic).sum();
    let sum_cog: u32 = functions.iter().map(|f| f.cognitive).sum();
    let max_cyc = functions.iter().map(|f| f.cyclomatic).max().unwrap_or(0);
    let max_cog = functions.iter().map(|f| f.cognitive).max().unwrap_or(0);
    let complex = functions.iter().filter(|f| f.cyclomatic > 10).count();
    let very_complex = functions.iter().filter(|f| f.cyclomatic > 20).count();
    let total_lines: u32 = functions.iter().map(|f| f.lines).sum();

    let hotspot = functions
        .iter()
        .max_by_key(|f| f.cyclomatic)
        .map(|f| f.name.clone());

    ComplexitySummary {
        total_functions: total,
        avg_cyclomatic: sum_cyc as f64 / total as f64,
        max_cyclomatic: max_cyc,
        avg_cognitive: sum_cog as f64 / total as f64,
        max_cognitive: max_cog,
        complex_functions: complex,
        very_complex_functions: very_complex,
        total_lines,
        hotspot,
    }
}

/// CULTRA-943: rate a function by both cyclomatic AND cognitive complexity.
/// Cyclomatic counts branches mechanically — a flat `match` with 50 arms
/// has CC 50 but is trivially readable. Cognitive captures nesting and
/// reads like "how hard would this be to hold in my head". We use cog as
/// a secondary downgrade: a function with CC > 20 but cog < 20 is almost
/// always a flat dispatch / data table / serde boilerplate, and should
/// rate `complex` rather than `very_complex` to cut audit noise.
fn rate_complexity(cyclomatic: u32, cognitive: u32) -> &'static str {
    let base = match cyclomatic {
        0..=5 => "simple",
        6..=10 => "moderate",
        11..=20 => "complex",
        _ => "very_complex",
    };
    // Downgrade very_complex → complex when cognitive says it's really flat.
    if base == "very_complex" && cognitive < 20 {
        return "complex";
    }
    base
}

/// Extract all functions and calculate complexity metrics
fn extract_functions_with_complexity(
    root: &tree_sitter::Node,
    content: &[u8],
    file_path: &str,
    language: &str,
) -> Vec<FunctionComplexity> {
    let mut functions = Vec::new();
    let mut cursor = root.walk();

    collect_functions(root, &mut cursor, content, file_path, language, &mut functions);

    // Sort by cyclomatic complexity descending
    functions.sort_by(|a, b| b.cyclomatic.cmp(&a.cyclomatic));
    functions
}

fn collect_functions(
    node: &tree_sitter::Node,
    cursor: &mut tree_sitter::TreeCursor,
    content: &[u8],
    file_path: &str,
    language: &str,
    out: &mut Vec<FunctionComplexity>,
) {
    let kind = node.kind();

    let is_function = match language {
        "go" => kind == "function_declaration" || kind == "method_declaration",
        "typescript" | "tsx" | "javascript" => {
            kind == "function_declaration"
                || kind == "method_definition"
                || kind == "arrow_function"
        }
        "python" => kind == "function_definition",
        "rust" => kind == "function_item",
        "terraform" => kind == "block",
        _ => false,
    };

    if is_function {
        let name = extract_function_name(node, content, language);
        let receiver = extract_receiver(node, content, language);
        let line_start = node.start_position().row as u32 + 1;
        let line_end = node.end_position().row as u32 + 1;
        let lines = line_end - line_start + 1;

        let cyclomatic = calculate_cyclomatic(node, content, language);
        let cognitive = calculate_cognitive(node, content, language, 0);

        out.push(FunctionComplexity {
            name,
            location: format!("{}:{}-{}", file_path, line_start, line_end),
            line_start,
            line_end,
            lines,
            cyclomatic,
            cognitive,
            receiver,
            rating: rate_complexity(cyclomatic, cognitive).to_string(),
        });
    }

    // Recurse into children
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            collect_functions(&child, cursor, content, file_path, language, out);
            if !cursor.goto_next_sibling() {
                break;
            }
        }
        cursor.goto_parent();
    }
}

/// Find the first child matching a given kind using cursor
fn find_child_by_kind<'a>(node: &'a tree_sitter::Node<'a>, kinds: &[&str]) -> Option<tree_sitter::Node<'a>> {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if kinds.contains(&child.kind()) {
            return Some(child);
        }
    }
    None
}

fn extract_function_name(node: &tree_sitter::Node, content: &[u8], language: &str) -> String {
    match language {
        "go" => {
            find_child_by_kind(node, &["identifier", "field_identifier"])
                .map(|n| node_text(&n, content).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string())
        }
        "rust" => {
            find_child_by_kind(node, &["identifier"])
                .map(|n| node_text(&n, content).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string())
        }
        "python" => {
            find_child_by_kind(node, &["identifier"])
                .map(|n| node_text(&n, content).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string())
        }
        "typescript" | "tsx" | "javascript" => {
            if node.kind() == "arrow_function" {
                if let Some(parent) = node.parent() {
                    if parent.kind() == "variable_declarator" {
                        if let Some(id) = find_child_by_kind(&parent, &["identifier"]) {
                            return node_text(&id, content).to_string();
                        }
                    }
                }
                return "<arrow>".to_string();
            }
            find_child_by_kind(node, &["identifier", "property_identifier"])
                .map(|n| node_text(&n, content).to_string())
                .unwrap_or_else(|| "<anonymous>".to_string())
        }
        "terraform" => {
            // Block name: "resource aws_instance.web" or "module vpc"
            let mut cursor = node.walk();
            let children: Vec<_> = node.children(&mut cursor).collect();
            let block_type = children.first()
                .filter(|c| c.kind() == "identifier")
                .map(|c| node_text(c, content).to_string())
                .unwrap_or_default();
            let labels: Vec<String> = children.iter()
                .filter(|c| c.kind() == "string_lit")
                .map(|c| node_text(c, content).trim_matches('"').to_string())
                .collect();
            if labels.is_empty() {
                block_type
            } else {
                format!("{} {}", block_type, labels.join("."))
            }
        }
        _ => "<unknown>".to_string(),
    }
}

fn extract_receiver(node: &tree_sitter::Node, content: &[u8], language: &str) -> Option<String> {
    if language != "go" || node.kind() != "method_declaration" {
        return None;
    }

    find_child_by_kind(node, &["parameter_list"])
        .map(|n| node_text(&n, content).to_string())
}

/// Calculate cyclomatic complexity (McCabe, 1976)
/// CC = 1 + number of decision points
fn calculate_cyclomatic(node: &tree_sitter::Node, content: &[u8], language: &str) -> u32 {
    let mut complexity: u32 = 1; // Base complexity
    count_decision_points(node, content, language, &mut complexity);
    complexity
}

fn count_decision_points(
    node: &tree_sitter::Node,
    content: &[u8],
    language: &str,
    count: &mut u32,
) {
    let kind = node.kind();

    // Decision points that add 1 to cyclomatic complexity
    let is_decision = match language {
        "go" => matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "expression_case"
                | "type_case"
                | "default_case"
                | "select_statement"
                | "communication_case"
        ),
        "typescript" | "tsx" | "javascript" => matches!(
            kind,
            "if_statement"
                | "for_statement"
                | "for_in_statement"
                | "while_statement"
                | "do_statement"
                | "switch_case"
                | "catch_clause"
                | "ternary_expression"
        ),
        "python" => matches!(
            kind,
            "if_statement"
                | "elif_clause"
                | "for_statement"
                | "while_statement"
                | "except_clause"
                | "conditional_expression"
                | "list_comprehension"
        ),
        "rust" => matches!(
            kind,
            "if_expression"
                | "for_expression"
                | "while_expression"
                | "loop_expression"
                | "match_arm"
                | "if_let_expression"
                | "while_let_expression"
        ),
        "terraform" => {
            // Nested blocks (dynamic, provisioner, lifecycle, connection) add complexity
            // Conditional expressions (?:) also count
            kind == "block" && node.parent().map_or(false, |p| p.kind() == "body" && p.parent().map_or(false, |pp| pp.kind() == "block"))
                || kind == "conditional"
        },
        _ => false,
    };

    if is_decision {
        *count += 1;
    }

    // Count logical operators (&&, ||) as additional decision points
    if kind == "binary_expression" {
        let op = node
            .child_by_field_name("operator")
            .map(|n| node_text(&n, content))
            .unwrap_or("");
        if op == "&&" || op == "||" {
            *count += 1;
        }
    }

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            // Don't recurse into nested function definitions
            let is_nested_fn = match language {
                "go" => child.kind() == "function_declaration" || child.kind() == "func_literal",
                "typescript" | "tsx" | "javascript" => {
                    child.kind() == "function_declaration"
                        || child.kind() == "arrow_function"
                        || child.kind() == "function_expression"
                }
                "python" => child.kind() == "function_definition",
                "rust" => child.kind() == "closure_expression",
                _ => false,
            };
            if !is_nested_fn {
                count_decision_points(&child, content, language, count);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }
}

/// Calculate cognitive complexity (Sonar, 2017)
/// Penalizes nesting more heavily than cyclomatic complexity
fn calculate_cognitive(
    node: &tree_sitter::Node,
    content: &[u8],
    language: &str,
    nesting: u32,
) -> u32 {
    let mut total: u32 = 0;
    let kind = node.kind();

    // Increments: control flow that adds to cognitive complexity
    let (is_increment, adds_nesting) = match language {
        "go" => match kind {
            "if_statement" | "for_statement" | "select_statement" => (true, true),
            "expression_switch_statement" | "type_switch_statement" => (true, true),
            _ => (false, false),
        },
        "typescript" | "tsx" | "javascript" => match kind {
            "if_statement" | "for_statement" | "for_in_statement" | "while_statement"
            | "do_statement" | "switch_statement" => (true, true),
            "ternary_expression" => (true, false),
            "catch_clause" => (true, true),
            _ => (false, false),
        },
        "python" => match kind {
            "if_statement" | "for_statement" | "while_statement" => (true, true),
            "except_clause" => (true, true),
            "conditional_expression" => (true, false),
            _ => (false, false),
        },
        "rust" => match kind {
            "if_expression" | "for_expression" | "while_expression" | "loop_expression"
            | "match_expression" => (true, true),
            "if_let_expression" | "while_let_expression" => (true, true),
            _ => (false, false),
        },
        _ => (false, false),
    };

    if is_increment {
        // Base increment (1) + nesting penalty
        total += 1 + nesting;
    }

    // Logical operator sequences: only penalize when operator changes
    if kind == "binary_expression" {
        let op = node
            .child_by_field_name("operator")
            .map(|n| node_text(&n, content))
            .unwrap_or("");
        if op == "&&" || op == "||" {
            // Check if parent is same operator type
            let parent_is_same = node.parent().map_or(false, |p| {
                p.kind() == "binary_expression"
                    && p.child_by_field_name("operator")
                        .map(|n| node_text(&n, content))
                        .unwrap_or("")
                        == op
            });
            if !parent_is_same {
                total += 1;
            }
        }
    }

    let new_nesting = if adds_nesting { nesting + 1 } else { nesting };

    let mut cursor = node.walk();
    if cursor.goto_first_child() {
        loop {
            let child = cursor.node();
            // Don't recurse into nested function definitions
            let is_nested_fn = match language {
                "go" => child.kind() == "function_declaration" || child.kind() == "func_literal",
                "typescript" | "tsx" | "javascript" => {
                    child.kind() == "function_declaration"
                        || child.kind() == "arrow_function"
                        || child.kind() == "function_expression"
                }
                "python" => child.kind() == "function_definition",
                "rust" => child.kind() == "closure_expression",
                _ => false,
            };
            if !is_nested_fn {
                total += calculate_cognitive(&child, content, language, new_nesting);
            }
            if !cursor.goto_next_sibling() {
                break;
            }
        }
    }

    total
}

fn node_text<'a>(node: &tree_sitter::Node, content: &'a [u8]) -> &'a str {
    std::str::from_utf8(&content[node.byte_range()]).unwrap_or("")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    // CULTRA-943: verify the cog < 20 secondary downgrade rule for the
    // rating function. Pure unit test — no file I/O needed.

    #[test]
    fn test_rate_complexity_simple_moderate_complex_very_complex() {
        assert_eq!(rate_complexity(1, 1), "simple");
        assert_eq!(rate_complexity(5, 5), "simple");
        assert_eq!(rate_complexity(6, 6), "moderate");
        assert_eq!(rate_complexity(10, 10), "moderate");
        assert_eq!(rate_complexity(11, 11), "complex");
        assert_eq!(rate_complexity(20, 20), "complex");
        // cog ≥ 20 → stays very_complex
        assert_eq!(rate_complexity(21, 20), "very_complex");
        assert_eq!(rate_complexity(50, 50), "very_complex");
    }

    #[test]
    fn test_rate_complexity_flat_high_branch_downgrades() {
        // CULTRA-943: high CC + low cog = flat dispatch / data table.
        // Should downgrade to "complex" rather than "very_complex".
        assert_eq!(rate_complexity(222, 1), "complex"); // tailwind static_map pattern
        assert_eq!(rate_complexity(56, 7), "complex");  // tools.rs call_tool dispatch
        assert_eq!(rate_complexity(60, 3), "complex");  // resolve_arbitrary match
        assert_eq!(rate_complexity(28, 1), "complex");  // serde deserialize
        assert_eq!(rate_complexity(37, 14), "complex"); // sizing_value

        // But cog at the threshold (20) should NOT downgrade
        assert_eq!(rate_complexity(25, 20), "very_complex");
    }

    #[test]
    fn test_simple_go_function() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.go");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            r#"package main

func simple() int {{
    return 1
}}

func moderate(x int) int {{
    if x > 0 {{
        if x > 10 {{
            return x * 2
        }}
        return x
    }}
    return 0
}}
"#
        )
        .unwrap();

        let result = analyze_complexity(path.to_str().unwrap()).unwrap();
        assert_eq!(result.functions.len(), 2);
        assert_eq!(result.summary.total_functions, 2);

        // Find the simple function
        let simple = result.functions.iter().find(|f| f.name == "simple").unwrap();
        assert_eq!(simple.cyclomatic, 1);
        assert_eq!(simple.rating, "simple");

        // Find the moderate function
        let moderate = result.functions.iter().find(|f| f.name == "moderate").unwrap();
        assert!(moderate.cyclomatic >= 3, "Expected CC >= 3 for nested ifs, got {}", moderate.cyclomatic);
    }

    #[test]
    fn test_js_arrow_function() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.js");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            r#"const handler = (req, res) => {{
    if (req.method === 'GET') {{
        res.send('ok');
    }} else {{
        res.status(405).send('not allowed');
    }}
}};
"#
        )
        .unwrap();

        let result = analyze_complexity(path.to_str().unwrap()).unwrap();
        assert!(!result.functions.is_empty(), "Expected at least one function");
    }

    #[test]
    fn test_python_complexity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.py");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            r#"def process(items):
    for item in items:
        if item.active:
            if item.value > threshold:
                result.append(item)
            elif item.fallback:
                fallback.append(item)
    return result
"#
        )
        .unwrap();

        let result = analyze_complexity(path.to_str().unwrap()).unwrap();
        assert_eq!(result.functions.len(), 1);
        let func = &result.functions[0];
        assert_eq!(func.name, "process");
        assert!(func.cyclomatic >= 4, "Expected CC >= 4, got {}", func.cyclomatic);
        assert!(func.cognitive > func.cyclomatic, "Cognitive should be higher than cyclomatic due to nesting");
    }

    #[test]
    fn test_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("empty.go");
        let mut f = fs::File::create(&path).unwrap();
        write!(f, "package main\n").unwrap();

        let result = analyze_complexity(path.to_str().unwrap()).unwrap();
        assert_eq!(result.functions.len(), 0);
        assert_eq!(result.summary.total_functions, 0);
    }

    #[test]
    fn test_rust_complexity() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.rs");
        let mut f = fs::File::create(&path).unwrap();
        write!(
            f,
            r#"fn handle_request(req: Request) -> Response {{
    match req.method {{
        Method::GET => {{
            if req.path == "/" {{
                Response::ok()
            }} else {{
                Response::not_found()
            }}
        }}
        Method::POST => Response::ok(),
        _ => Response::method_not_allowed(),
    }}
}}
"#
        )
        .unwrap();

        let result = analyze_complexity(path.to_str().unwrap()).unwrap();
        assert_eq!(result.functions.len(), 1);
        let func = &result.functions[0];
        assert_eq!(func.name, "handle_request");
        assert!(func.cyclomatic >= 4, "Expected CC >= 4 for match + if, got {}", func.cyclomatic);
    }
}
