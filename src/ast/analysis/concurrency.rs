use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use tree_sitter::{Query, QueryCursor};

use streaming_iterator::StreamingIterator;

/// Complete concurrency analysis for Go code
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConcurrencyAnalysis {
    pub goroutines: Vec<GoroutineSpawn>,
    pub channels: Vec<ChannelInfo>,
    pub mutexes: Vec<MutexInfo>,
    pub race_conditions: Vec<RaceCondition>,
    pub deadlock_risks: Vec<DeadlockRisk>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub select_statements: Vec<SelectStatement>,
}

/// Goroutine spawn site with pattern analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GoroutineSpawn {
    pub location: String,
    pub spawned_in: String,
    pub pattern: String, // "select_loop", "ticker_loop", "worker_loop", "fire_and_forget"
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub channels_used: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub issues: Vec<String>,
}

/// Channel information with buffer details
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub channel_type: String,
    pub location: String,
    pub buffered: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub buffer_size: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<String>, // "send", "receive", "bidirectional"
}

/// Mutex/RWMutex usage information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MutexInfo {
    pub name: String,
    #[serde(rename = "type")]
    pub mutex_type: String,
    pub location: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub protects: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub lock_sites: Vec<String>,
}

/// Potential race condition warning
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RaceCondition {
    #[serde(rename = "type")]
    pub race_type: String, // "unprotected_write", "unprotected_read", "shared_map_access"
    pub variable: String,
    pub location: String,
    pub severity: String, // "error", "warning", "info"
    pub description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

/// Potential deadlock risk
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeadlockRisk {
    #[serde(rename = "type")]
    pub risk_type: String, // "circular_lock", "missing_unlock", "channel_deadlock"
    pub locations: Vec<String>,
    pub description: String,
    pub severity: String,
}

/// Select statement with case analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SelectStatement {
    pub location: String,
    pub cases: Vec<String>,
    pub has_default: bool,
}

/// Analyze Go code for concurrency patterns and potential issues
pub fn analyze_concurrency(file_path: &str) -> Result<ConcurrencyAnalysis> {
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

    // Extract concurrency primitives
    let goroutines = extract_goroutine_spawns(&root_node, content_bytes)?;
    let channels = extract_channels(&root_node, content_bytes)?;
    let mutexes = extract_mutexes(&root_node, content_bytes)?;
    let select_statements = extract_select_statements(&root_node, content_bytes)?;

    // Detect issues
    let race_conditions = detect_race_conditions(&root_node, content_bytes, &goroutines, &mutexes)?;
    let deadlock_risks = detect_deadlock_risks(&select_statements);

    Ok(ConcurrencyAnalysis {
        goroutines,
        channels,
        mutexes,
        race_conditions,
        deadlock_risks,
        select_statements,
    })
}

/// Extract all goroutine spawn sites
fn extract_goroutine_spawns(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<GoroutineSpawn>> {
    let mut spawns = Vec::new();

    let query_source = r#"
    (go_statement
        (call_expression) @go.call) @go.stmt
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut go_stmt: Option<tree_sitter::Node> = None;
        let mut call_expr: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "go.stmt" => go_stmt = Some(capture.node),
                "go.call" => call_expr = Some(capture.node),
                _ => {}
            }
        }

        if let Some(stmt_node) = go_stmt {
            let enclosing_func = find_enclosing_function(&stmt_node, content);
            let pattern = if let Some(call_node) = call_expr {
                classify_goroutine_pattern(&call_node, content)
            } else {
                "unknown".to_string()
            };
            let channels_used = if let Some(call_node) = call_expr {
                extract_channels_in_scope(&call_node, content)
            } else {
                Vec::new()
            };

            spawns.push(GoroutineSpawn {
                location: format_location(&stmt_node),
                spawned_in: enclosing_func,
                pattern,
                channels_used,
                issues: Vec::new(),
            });
        }
    }

    Ok(spawns)
}

/// Extract channel declarations and make() calls
fn extract_channels(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<ChannelInfo>> {
    let mut channels = Vec::new();

    // Query for channel variable declarations
    let var_query = r#"
    (var_declaration
        (var_spec
            name: (identifier) @chan.name
            type: (channel_type) @chan.type)) @chan.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, var_query)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut chan_name = String::new();
        let mut chan_type = String::new();
        let mut chan_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "chan.name" => {
                    chan_name = capture.node.utf8_text(content)?.to_string();
                }
                "chan.type" => {
                    chan_type = capture.node.utf8_text(content)?.to_string();
                }
                "chan.decl" => {
                    chan_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if !chan_name.is_empty() && chan_node.is_some() {
            channels.push(ChannelInfo {
                name: chan_name,
                channel_type: chan_type.clone(),
                location: format_location(&chan_node.unwrap()),
                buffered: false,
                buffer_size: None,
                direction: Some(detect_channel_direction(&chan_type)),
            });
        }
    }

    // Query for make(chan ...) calls
    let make_query = r#"
    (call_expression
        function: (identifier) @func.name
        arguments: (argument_list
            (channel_type) @chan.type)) @make.call
    "#;

    let query = Query::new(&language, make_query)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut func_name = String::new();
        let mut chan_type = String::new();
        let mut make_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "func.name" => {
                    func_name = capture.node.utf8_text(content)?.to_string();
                }
                "chan.type" => {
                    chan_type = capture.node.utf8_text(content)?.to_string();
                }
                "make.call" => {
                    make_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if func_name == "make" && make_node.is_some() {
            let (buffered, buffer_size) = detect_buffered_channel(&make_node.unwrap(), content);

            channels.push(ChannelInfo {
                name: "anonymous".to_string(),
                channel_type: chan_type.clone(),
                location: format_location(&make_node.unwrap()),
                buffered,
                buffer_size,
                direction: Some(detect_channel_direction(&chan_type)),
            });
        }
    }

    Ok(channels)
}

/// Extract mutex/RWMutex declarations
fn extract_mutexes(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<MutexInfo>> {
    let mut mutexes = Vec::new();

    let query_source = r#"
    (field_declaration
        name: (field_identifier) @mutex.name
        type: (qualified_type
            package: (package_identifier) @pkg.name
            name: (type_identifier) @type.name)) @mutex.decl
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut mutex_name = String::new();
        let mut pkg_name = String::new();
        let mut type_name = String::new();
        let mut mutex_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "mutex.name" => {
                    mutex_name = capture.node.utf8_text(content)?.to_string();
                }
                "pkg.name" => {
                    pkg_name = capture.node.utf8_text(content)?.to_string();
                }
                "type.name" => {
                    type_name = capture.node.utf8_text(content)?.to_string();
                }
                "mutex.decl" => {
                    mutex_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if pkg_name == "sync" && (type_name == "Mutex" || type_name == "RWMutex") {
            let full_type = format!("{}.{}", pkg_name, type_name);
            let lock_sites = find_lock_sites(root_node, content, &mutex_name)?;

            mutexes.push(MutexInfo {
                name: mutex_name,
                mutex_type: full_type,
                location: format_location(&mutex_node.unwrap()),
                protects: Vec::new(), // Would need data flow analysis
                lock_sites,
            });
        }
    }

    Ok(mutexes)
}

/// Extract select statements with their cases
fn extract_select_statements(
    root_node: &tree_sitter::Node,
    content: &[u8],
) -> Result<Vec<SelectStatement>> {
    let mut selects = Vec::new();

    let query_source = r#"
    (select_statement) @select.stmt
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            if *capture_name == "select.stmt" {
                let select_stmt = capture.node;
                let mut cases = Vec::new();
                let mut has_default = false;

                // Iterate through children to find cases
                for i in 0..select_stmt.child_count() {
                    if let Some(child) = select_stmt.child(i as u32) {
                        match child.kind() {
                            "communication_case" => {
                                if let Ok(case_content) = child.utf8_text(content) {
                                    cases.push(case_content.to_string());
                                }
                            }
                            "default_case" => {
                                has_default = true;
                                cases.push("default".to_string());
                            }
                            _ => {}
                        }
                    }
                }

                selects.push(SelectStatement {
                    location: format_location(&select_stmt),
                    cases,
                    has_default,
                });
            }
        }
    }

    Ok(selects)
}

/// Detect potential race conditions
fn detect_race_conditions(
    root_node: &tree_sitter::Node,
    content: &[u8],
    goroutines: &[GoroutineSpawn],
    mutexes: &[MutexInfo],
) -> Result<Vec<RaceCondition>> {
    let mut races = Vec::new();

    // Only check for races if there are goroutines
    if goroutines.is_empty() {
        return Ok(races);
    }

    // Detect map access without locks
    let map_query = r#"
    (index_expression
        operand: (identifier) @map.name
        index: (_)) @map.access
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, map_query)?;
    let mut cursor = QueryCursor::new();

    let mut map_accesses: HashMap<String, Vec<String>> = HashMap::new();

    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut map_name = String::new();
        let mut access_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "map.name" => {
                    map_name = capture.node.utf8_text(content)?.to_string();
                }
                "map.access" => {
                    access_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if !map_name.is_empty() && access_node.is_some() {
            if is_map_type(root_node, content, &map_name)? {
                let location = format_location(&access_node.unwrap());
                map_accesses.entry(map_name).or_insert_with(Vec::new).push(location);
            }
        }
    }

    // Check for unprotected map access
    for (map_name, locations) in map_accesses {
        if locations.len() > 1 {
            // Check if protected by mutex
            let protected = mutexes.iter().any(|m| !m.lock_sites.is_empty());

            if !protected {
                races.push(RaceCondition {
                    race_type: "shared_map_access".to_string(),
                    variable: map_name,
                    location: locations[0].clone(),
                    severity: "warning".to_string(),
                    description: "Map accessed in multiple locations with goroutines present, but no mutex detected".to_string(),
                    fix: Some("Protect map access with sync.Mutex or sync.RWMutex".to_string()),
                });
            }
        }
    }

    Ok(races)
}

/// Detect potential deadlock risks
fn detect_deadlock_risks(select_statements: &[SelectStatement]) -> Vec<DeadlockRisk> {
    let mut risks = Vec::new();

    // Check for select without default (can deadlock if all channels block)
    for sel in select_statements {
        if !sel.has_default && !sel.cases.is_empty() {
            risks.push(DeadlockRisk {
                risk_type: "channel_deadlock".to_string(),
                locations: vec![sel.location.clone()],
                description: "Select statement without default case can deadlock if all channels block".to_string(),
                severity: "info".to_string(),
            });
        }
    }

    risks
}

// Helper functions

fn find_enclosing_function(node: &tree_sitter::Node, content: &[u8]) -> String {
    let mut current = node.parent();
    while let Some(parent) = current {
        if parent.kind() == "function_declaration" || parent.kind() == "method_declaration" {
            // Find the function name
            for i in 0..parent.child_count() {
                if let Some(child) = parent.child(i as u32) {
                    if child.kind() == "identifier" || child.kind() == "field_identifier" {
                        if let Ok(name) = child.utf8_text(content) {
                            return name.to_string();
                        }
                    }
                }
            }
        }
        current = parent.parent();
    }
    "unknown".to_string()
}

fn classify_goroutine_pattern(node: &tree_sitter::Node, content: &[u8]) -> String {
    if let Ok(code) = node.utf8_text(content) {
        if code.contains("select {") {
            return "select_loop".to_string();
        }
        if code.contains("time.Tick") || code.contains("time.NewTicker") {
            return "ticker_loop".to_string();
        }
        if code.contains("for {") || code.contains("for range") {
            return "worker_loop".to_string();
        }
    }
    "fire_and_forget".to_string()
}

fn extract_channels_in_scope(node: &tree_sitter::Node, content: &[u8]) -> Vec<String> {
    let mut channels = Vec::new();

    if let Ok(code) = node.utf8_text(content) {
        for line in code.lines() {
            if line.contains("<-") {
                // Extract channel name (simplified)
                let parts: Vec<&str> = line.split("<-").collect();
                if !parts.is_empty() {
                    let chan_name = parts[0].trim();
                    if !chan_name.is_empty() && !chan_name.contains('(') {
                        channels.push(chan_name.to_string());
                    }
                }
            }
        }
    }

    channels
}

fn detect_channel_direction(chan_type: &str) -> String {
    if chan_type.contains("<-chan") {
        "receive".to_string()
    } else if chan_type.contains("chan<-") {
        "send".to_string()
    } else {
        "bidirectional".to_string()
    }
}

fn detect_buffered_channel(make_node: &tree_sitter::Node, _content: &[u8]) -> (bool, Option<i32>) {
    // Find argument list
    for i in 0..make_node.child_count() {
        if let Some(child) = make_node.child(i as u32) {
            if child.kind() == "argument_list" {
                // Count non-comma arguments
                let mut arg_count = 0;
                for j in 0..child.child_count() {
                    if let Some(arg_child) = child.child(j as u32) {
                        if arg_child.kind() != "," {
                            arg_count += 1;
                        }
                    }
                }

                // If more than 1 argument (channel type + buffer size), it's buffered
                if arg_count > 1 {
                    // TODO: Extract actual buffer size value (requires constant evaluation)
                    return (true, None);
                }
            }
        }
    }

    (false, None)
}

fn find_lock_sites(
    root_node: &tree_sitter::Node,
    content: &[u8],
    mutex_name: &str,
) -> Result<Vec<String>> {
    let mut sites = Vec::new();

    let query_source = r#"
    (call_expression
        function: (selector_expression
            operand: (identifier) @obj.name
            field: (field_identifier) @method.name)) @call
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        let mut obj_name = String::new();
        let mut method_name = String::new();
        let mut call_node: Option<tree_sitter::Node> = None;

        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            match *capture_name {
                "obj.name" => {
                    obj_name = capture.node.utf8_text(content)?.to_string();
                }
                "method.name" => {
                    method_name = capture.node.utf8_text(content)?.to_string();
                }
                "call" => {
                    call_node = Some(capture.node);
                }
                _ => {}
            }
        }

        if obj_name == mutex_name
            && (method_name == "Lock"
                || method_name == "Unlock"
                || method_name == "RLock"
                || method_name == "RUnlock")
        {
            sites.push(format_location(&call_node.unwrap()));
        }
    }

    Ok(sites)
}

fn is_map_type(root_node: &tree_sitter::Node, content: &[u8], var_name: &str) -> Result<bool> {
    let query_source = r#"
    (var_declaration
        (var_spec
            name: (identifier) @var.name
            type: (map_type) @map.type))
    "#;

    let language = tree_sitter_go::LANGUAGE.into();
    let query = Query::new(&language, query_source)?;
    let mut cursor = QueryCursor::new();
    let mut matches = cursor.matches(&query, *root_node, content);
    while let Some(match_) = matches.next() {
        for capture in match_.captures {
            let capture_name = &query.capture_names()[capture.index as usize];
            if *capture_name == "var.name" {
                if let Ok(found_name) = capture.node.utf8_text(content) {
                    if found_name == var_name {
                        return Ok(true);
                    }
                }
            }
        }
    }

    Ok(false)
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
    fn test_concurrency_analysis() {
        let test_code = r#"package main

import (
	"sync"
	"time"
)

type Server struct {
	mu      sync.RWMutex
	clients map[string]int
	done    chan bool
}

func (s *Server) Run() {
	// Goroutine with select loop
	go func() {
		ticker := time.NewTicker(5 * time.Second)
		defer ticker.Stop()

		for {
			select {
			case <-ticker.C:
				s.cleanup()
			case <-s.done:
				return
			}
		}
	}()
}

func (s *Server) cleanup() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.clients = make(map[string]int)
}

// Race condition - accessing map without lock
func (s *Server) UnsafeAccess() {
	_ = s.clients["test"]
}

// Potential deadlock - select without default
func waitForever() {
	ch := make(chan int)
	select {
	case <-ch:
		println("never happens")
	}
}

// Buffered channel
func createBuffered() chan int {
	return make(chan int, 100)
}
"#;

        // Write to temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_concurrency.go");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(test_code.as_bytes())
            .expect("Failed to write temp file");

        // Analyze
        let result = analyze_concurrency(test_file.to_str().unwrap());
        assert!(result.is_ok(), "Analysis should succeed");

        let analysis = result.unwrap();

        // Verify goroutines detected
        assert!(
            !analysis.goroutines.is_empty(),
            "Should detect goroutines"
        );

        // Verify channels detected
        assert!(
            !analysis.channels.is_empty(),
            "Should detect channels"
        );

        // Verify mutexes detected
        assert!(
            !analysis.mutexes.is_empty(),
            "Should detect mutexes"
        );

        // Verify select statements detected
        assert!(
            !analysis.select_statements.is_empty(),
            "Should detect select statements"
        );

        // Verify specific patterns
        let has_select_loop = analysis
            .goroutines
            .iter()
            .any(|g| g.pattern == "select_loop");
        assert!(has_select_loop, "Should find select_loop pattern");

        // Verify mutex type
        let has_rwmutex = analysis
            .mutexes
            .iter()
            .any(|m| m.mutex_type == "sync.RWMutex");
        assert!(has_rwmutex, "Should find sync.RWMutex");

        // Clean up
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_channel_direction_detection() {
        assert_eq!(detect_channel_direction("chan int"), "bidirectional");
        assert_eq!(detect_channel_direction("<-chan int"), "receive");
        assert_eq!(detect_channel_direction("chan<- int"), "send");
    }

    #[test]
    fn test_goroutine_pattern_classification() {
        let mut parser = tree_sitter::Parser::new();
        let lang = tree_sitter_go::LANGUAGE.into();
        parser
            .set_language(&lang)
            .expect("Failed to set language");

        // Test select_loop pattern
        let code = "select { case <-ch: }";
        let tree = parser.parse(code, None).expect("Failed to parse");
        let root = tree.root_node();
        assert_eq!(
            classify_goroutine_pattern(&root, code.as_bytes()),
            "select_loop"
        );

        // Test ticker_loop pattern
        let code2 = "time.NewTicker(5 * time.Second)";
        let tree2 = parser.parse(code2, None).expect("Failed to parse");
        let root2 = tree2.root_node();
        assert_eq!(
            classify_goroutine_pattern(&root2, code2.as_bytes()),
            "ticker_loop"
        );
    }
}
