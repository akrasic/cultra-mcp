use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;

/// Complete CSS file analysis
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssAnalysis {
    pub file_path: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<CssRule>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub variables: Vec<CssVariable>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub media_queries: Vec<MediaQuery>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub keyframes: Vec<KeyframeBlock>,
    pub stats: CssStats,
}

/// A CSS rule block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssRule {
    pub selectors: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub properties: Vec<CssProperty>,
    pub start_line: usize,
    pub end_line: usize,
    pub specificity: Specificity,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_nested: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent_context: Option<String>,
}

/// A CSS property declaration
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssProperty {
    pub name: String,
    pub value: String,
    pub line: usize,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub is_important: bool,
}

/// A CSS custom property (variable) definition
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssVariable {
    pub name: String,
    pub value: String,
    pub line: usize,
    pub scope: String,
}

/// A @media query block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaQuery {
    pub condition: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<CssRule>,
    pub start_line: usize,
    pub end_line: usize,
}

/// A @keyframes block
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyframeBlock {
    pub name: String,
    pub start_line: usize,
    pub end_line: usize,
}

/// CSS selector specificity
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Specificity {
    pub ids: u8,
    pub classes: u8,
    pub elements: u8,
    pub display: String,
}

/// Summary statistics for a CSS file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssStats {
    pub total_rules: usize,
    pub total_properties: usize,
    pub total_variables: usize,
    pub total_media_queries: usize,
    pub total_keyframes: usize,
    pub important_count: usize,
}

/// An unused CSS selector found by cross-referencing
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnusedSelector {
    pub selector: String,
    pub start_line: usize,
    pub end_line: usize,
    pub file_path: String,
}

// ========== Parser State Machine ==========

#[derive(Debug, Clone, PartialEq)]
enum BlockType {
    Rule,
    Media(String),   // condition
    Keyframes(String), // name
    FontFace,
    Other,
}

struct ParserState {
    in_comment: bool,
    brace_depth: usize,
    // Current block tracking
    block_stack: Vec<BlockEntry>,
    // Accumulated selector text before {
    selector_buf: String,
    selector_start_line: usize,
    // Results
    rules: Vec<CssRule>,
    variables: Vec<CssVariable>,
    media_queries: Vec<MediaQuery>,
    keyframes: Vec<KeyframeBlock>,
    important_count: usize,
    total_properties: usize,
}

struct BlockEntry {
    block_type: BlockType,
    start_line: usize,
    selectors: Vec<String>,
    properties: Vec<CssProperty>,
    // For media queries: nested rules
    nested_rules: Vec<CssRule>,
    parent_context: Option<String>,
}

impl ParserState {
    fn new() -> Self {
        Self {
            in_comment: false,
            brace_depth: 0,
            block_stack: Vec::new(),
            selector_buf: String::new(),
            selector_start_line: 0,
            rules: Vec::new(),
            variables: Vec::new(),
            media_queries: Vec::new(),
            keyframes: Vec::new(),
            important_count: 0,
            total_properties: 0,
        }
    }
}

/// Analyze a CSS file and extract structural metadata
pub fn analyze_css(file_path: &str) -> Result<CssAnalysis> {
    let content = fs::read_to_string(file_path)?;
    let mut state = ParserState::new();

    for (line_idx, line) in content.lines().enumerate() {
        let line_num = line_idx + 1;
        parse_line(&mut state, line, line_num);
    }

    let total_rules = state.rules.len()
        + state.media_queries.iter().map(|m| m.rules.len()).sum::<usize>();

    let stats = CssStats {
        total_rules,
        total_properties: state.total_properties,
        total_variables: state.variables.len(),
        total_media_queries: state.media_queries.len(),
        total_keyframes: state.keyframes.len(),
        important_count: state.important_count,
    };

    Ok(CssAnalysis {
        file_path: file_path.to_string(),
        rules: state.rules,
        variables: state.variables,
        media_queries: state.media_queries,
        keyframes: state.keyframes,
        stats,
    })
}

/// Find CSS rules matching a selector pattern (substring match)
pub fn find_css_rules(file_path: &str, selector_pattern: &str) -> Result<Vec<CssRule>> {
    let analysis = analyze_css(file_path)?;
    let pattern_lower = selector_pattern.to_lowercase();

    let mut matches = Vec::new();

    for rule in &analysis.rules {
        if rule.selectors.iter().any(|s| s.to_lowercase().contains(&pattern_lower)) {
            matches.push(rule.clone());
        }
    }

    for mq in &analysis.media_queries {
        for rule in &mq.rules {
            if rule.selectors.iter().any(|s| s.to_lowercase().contains(&pattern_lower)) {
                matches.push(rule.clone());
            }
        }
    }

    Ok(matches)
}

/// Find CSS selectors not referenced in component files
pub fn find_unused_selectors(css_path: &str, component_paths: &[&str]) -> Result<Vec<UnusedSelector>> {
    let analysis = analyze_css(css_path)?;

    // Read all component files into one big string for searching
    let mut component_content = String::new();
    for path in component_paths {
        if let Ok(content) = fs::read_to_string(path) {
            component_content.push_str(&content);
            component_content.push('\n');
        }
    }

    let mut unused = Vec::new();

    let check_rules = |rules: &[CssRule], unused: &mut Vec<UnusedSelector>| {
        for rule in rules {
            for selector in &rule.selectors {
                let class_names = extract_class_names(selector);
                for class_name in &class_names {
                    if !component_content.contains(class_name) {
                        unused.push(UnusedSelector {
                            selector: selector.clone(),
                            start_line: rule.start_line,
                            end_line: rule.end_line,
                            file_path: css_path.to_string(),
                        });
                        break; // one unused class is enough to flag the selector
                    }
                }
            }
        }
    };

    check_rules(&analysis.rules, &mut unused);
    for mq in &analysis.media_queries {
        check_rules(&mq.rules, &mut unused);
    }

    Ok(unused)
}

// ========== Parser internals ==========

fn parse_line(state: &mut ParserState, line: &str, line_num: usize) {
    let mut chars = line.char_indices().peekable();

    while let Some(&(bp, _)) = chars.peek() {
        // Handle multi-line comments
        if state.in_comment {
            if let Some(end) = line[bp..].find("*/") {
                state.in_comment = false;
                let target = bp + end + 2;
                while let Some(&(b, _)) = chars.peek() {
                    if b >= target { break; }
                    chars.next();
                }
                continue;
            } else {
                return; // rest of line is comment
            }
        }

        // Check for comment start
        if line[bp..].starts_with("/*") {
            if let Some(end) = line[bp + 2..].find("*/") {
                // Single-line comment — skip past it
                let target = bp + end + 4;
                while let Some(&(b, _)) = chars.peek() {
                    if b >= target { break; }
                    chars.next();
                }
                continue;
            } else {
                state.in_comment = true;
                return;
            }
        }

        // Skip single-line // comments (not standard CSS but sometimes seen)
        if line[bp..].starts_with("//") {
            return;
        }

        // unwrap: chars.peek() at the top of the loop already returned Some,
        // so the iterator has at least one more element.
        let (_, ch) = chars.next().unwrap();

        match ch {
            '{' => {
                let selector_text = state.selector_buf.trim().to_string();
                let start_line = if state.selector_start_line > 0 {
                    state.selector_start_line
                } else {
                    line_num
                };

                let block_type = classify_block(&selector_text);
                let parent_ctx = state.block_stack.last().and_then(|b| {
                    match &b.block_type {
                        BlockType::Media(cond) => Some(format!("@media {}", cond)),
                        _ => None,
                    }
                });

                let selectors = if matches!(block_type, BlockType::Rule) {
                    parse_selectors(&selector_text)
                } else {
                    Vec::new()
                };

                state.block_stack.push(BlockEntry {
                    block_type,
                    start_line,
                    selectors,
                    properties: Vec::new(),
                    nested_rules: Vec::new(),
                    parent_context: parent_ctx,
                });

                state.brace_depth += 1;
                state.selector_buf.clear();
                state.selector_start_line = 0;
            }
            '}' => {
                if state.brace_depth > 0 {
                    state.brace_depth -= 1;
                }

                if let Some(block) = state.block_stack.pop() {
                    finish_block(state, block, line_num);
                }

                state.selector_buf.clear();
                state.selector_start_line = 0;
            }
            ';' if !state.block_stack.is_empty() => {
                // Property inside a block
                let prop_text = state.selector_buf.trim().to_string();
                state.selector_buf.clear();
                state.selector_start_line = 0;

                if !prop_text.is_empty() {
                    if let Some(block) = state.block_stack.last_mut() {
                        if let Some(prop) = parse_property(&prop_text, line_num) {
                            if prop.is_important {
                                state.important_count += 1;
                            }
                            // Track CSS variables
                            if prop.name.starts_with("--") {
                                let scope = if block.selectors.is_empty() {
                                    ":root".to_string()
                                } else {
                                    block.selectors.join(", ")
                                };
                                state.variables.push(CssVariable {
                                    name: prop.name.clone(),
                                    value: prop.value.clone(),
                                    line: line_num,
                                    scope,
                                });
                            }
                            state.total_properties += 1;
                            block.properties.push(prop);
                        }
                    }
                }
            }
            _ => {
                if state.selector_buf.is_empty() && !ch.is_whitespace() {
                    state.selector_start_line = line_num;
                }
                state.selector_buf.push(ch);
            }
        }
    }

    // If we're not inside a block, add space between lines for multi-line selectors
    if state.brace_depth == 0 && !state.selector_buf.is_empty() && !state.selector_buf.ends_with(' ') {
        state.selector_buf.push(' ');
    }

}

fn classify_block(selector: &str) -> BlockType {
    let trimmed = selector.trim();
    if trimmed.starts_with("@media") {
        let cond = trimmed.strip_prefix("@media").unwrap_or("").trim().to_string();
        BlockType::Media(cond)
    } else if trimmed.starts_with("@keyframes") || trimmed.starts_with("@-webkit-keyframes") {
        let name = trimmed
            .split_whitespace()
            .nth(1)
            .unwrap_or("unknown")
            .to_string();
        BlockType::Keyframes(name)
    } else if trimmed.starts_with("@font-face") {
        BlockType::FontFace
    } else if trimmed.starts_with('@') {
        // @supports, @layer, @container, etc.
        BlockType::Other
    } else {
        BlockType::Rule
    }
}

fn finish_block(state: &mut ParserState, block: BlockEntry, end_line: usize) {
    match block.block_type {
        BlockType::Rule => {
            if !block.selectors.is_empty() {
                let specificity = compute_max_specificity(&block.selectors);
                let rule = CssRule {
                    selectors: block.selectors,
                    properties: block.properties,
                    start_line: block.start_line,
                    end_line,
                    specificity,
                    is_nested: block.parent_context.is_some(),
                    parent_context: block.parent_context,
                };
                // If inside a media query, add to parent's nested rules
                if let Some(parent) = state.block_stack.last_mut() {
                    if matches!(parent.block_type, BlockType::Media(_)) {
                        parent.nested_rules.push(rule);
                        return;
                    }
                }
                state.rules.push(rule);
            }
        }
        BlockType::Media(condition) => {
            state.media_queries.push(MediaQuery {
                condition,
                rules: block.nested_rules,
                start_line: block.start_line,
                end_line,
            });
        }
        BlockType::Keyframes(name) => {
            state.keyframes.push(KeyframeBlock {
                name,
                start_line: block.start_line,
                end_line,
            });
        }
        BlockType::FontFace | BlockType::Other => {
            // Store font-face as a rule with @font-face selector
            if !block.properties.is_empty() {
                let selector = match &block.block_type {
                    BlockType::FontFace => "@font-face".to_string(),
                    _ => "@rule".to_string(),
                };
                let rule = CssRule {
                    selectors: vec![selector],
                    properties: block.properties,
                    start_line: block.start_line,
                    end_line,
                    specificity: Specificity {
                        ids: 0,
                        classes: 0,
                        elements: 0,
                        display: "0-0-0".to_string(),
                    },
                    is_nested: false,
                    parent_context: None,
                };
                state.rules.push(rule);
            }
        }
    }
}

fn parse_selectors(text: &str) -> Vec<String> {
    text.split(',')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

fn parse_property(text: &str, line: usize) -> Option<CssProperty> {
    let colon_pos = text.find(':')?;
    let name = text[..colon_pos].trim().to_string();
    let mut value = text[colon_pos + 1..].trim().to_string();

    if name.is_empty() {
        return None;
    }

    let is_important = value.contains("!important");
    if is_important {
        value = value.replace("!important", "").trim().to_string();
    }

    // Remove trailing semicolons that might have slipped through
    value = value.trim_end_matches(';').trim().to_string();

    Some(CssProperty {
        name,
        value,
        line,
        is_important,
    })
}

/// Compute specificity for a single selector
fn compute_specificity(selector: &str) -> Specificity {
    let s = selector.trim();
    let mut classes: u8 = 0;
    let mut elements: u8 = 0;

    // Remove pseudo-element content (::before, ::after, etc.)
    // Count them as element selectors
    let mut working = s.to_string();

    // Count and remove ::pseudo-elements (must be before single : check)
    let pseudo_element_count = working.matches("::").count() as u8;
    elements = elements.saturating_add(pseudo_element_count);
    working = working.replace("::", " ");

    // Count #id selectors
    let ids = working.matches('#').count() as u8;

    // Count . class selectors
    let class_count = working.matches('.').count() as u8;
    classes = classes.saturating_add(class_count);

    // Count [attr] selectors
    let attr_count = working.matches('[').count() as u8;
    classes = classes.saturating_add(attr_count);

    // Count :pseudo-class selectors (single colon remaining after :: removal)
    // Exclude :root, :not, :where, :is which don't add specificity or are special
    let pseudo_classes: u8 = working.matches(':').count() as u8;
    // Subtract pseudo-classes that don't contribute (:where)
    let where_count = working.matches(":where").count() as u8;
    classes = classes.saturating_add(pseudo_classes.saturating_sub(where_count));

    // Count element selectors (type selectors like div, span, etc.)
    // Split by combinators and spaces, count tokens that look like element names
    for token in working.split(|c: char| c.is_whitespace() || c == '>' || c == '+' || c == '~') {
        let token = token.trim();
        if token.is_empty() {
            continue;
        }
        // Extract the base element name (before any class, id, attr, or pseudo)
        let base = token
            .split(|c: char| c == '.' || c == '#' || c == '[' || c == ':')
            .next()
            .unwrap_or("");
        if !base.is_empty() && base != "*" && base.chars().next().map_or(false, |c| c.is_alphabetic()) {
            elements = elements.saturating_add(1);
        }
    }

    Specificity {
        display: format!("{}-{}-{}", ids, classes, elements),
        ids,
        classes,
        elements,
    }
}

/// Compute the maximum specificity across multiple selectors in a rule
fn compute_max_specificity(selectors: &[String]) -> Specificity {
    selectors
        .iter()
        .map(|s| compute_specificity(s))
        .max_by_key(|sp| (sp.ids, sp.classes, sp.elements))
        .unwrap_or(Specificity {
            ids: 0,
            classes: 0,
            elements: 0,
            display: "0-0-0".to_string(),
        })
}

/// Extract class names from a CSS selector for unused-selector detection
fn extract_class_names(selector: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut chars = selector.chars().peekable();

    while let Some(&ch) = chars.peek() {
        if ch == '.' {
            chars.next(); // consume the dot
            let mut name = String::new();
            while let Some(&c) = chars.peek() {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    name.push(c);
                    chars.next();
                } else {
                    break;
                }
            }
            if !name.is_empty() {
                names.push(name);
            }
        } else {
            chars.next();
        }
    }

    names
}

// ========== CSS Variable Dependency Graph ==========

/// Complete variable dependency graph for a CSS file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CssVariableGraph {
    pub file_path: String,
    pub variables: Vec<VariableNode>,
    pub edges: Vec<VariableEdge>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub leaves: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub cycles: Vec<Vec<String>>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub unresolved: Vec<UnresolvedReference>,
    pub stats: VariableGraphStats,
}

/// A node in the variable dependency graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableNode {
    pub name: String,
    pub value: String,
    pub line: usize,
    pub scope: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub referenced_by: Vec<String>,
}

/// A directed edge: `from` depends on `to`
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableEdge {
    pub from: String,
    pub to: String,
}

/// A var() reference to a variable that doesn't exist in this file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnresolvedReference {
    pub referencing_variable: String,
    pub referenced_name: String,
    pub line: usize,
}

/// Summary statistics for the variable graph
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VariableGraphStats {
    pub total_variables: usize,
    pub total_edges: usize,
    pub root_count: usize,
    pub leaf_count: usize,
    pub cycle_count: usize,
    pub unresolved_count: usize,
    pub max_depth: usize,
}

/// Build a dependency graph of CSS custom properties in a file
pub fn css_variable_graph(file_path: &str) -> Result<CssVariableGraph> {
    let analysis = analyze_css(file_path)?;

    // Build a set of all defined variable names
    let defined: std::collections::HashSet<String> = analysis
        .variables
        .iter()
        .map(|v| v.name.clone())
        .collect();

    // Build nodes with dependency info
    let mut nodes: Vec<VariableNode> = Vec::new();
    let mut edges: Vec<VariableEdge> = Vec::new();
    let mut unresolved: Vec<UnresolvedReference> = Vec::new();

    // Map from variable name to index for referenced_by tracking
    let mut name_to_idx: HashMap<String, usize> = HashMap::new();

    for var in &analysis.variables {
        let refs = extract_var_references(&var.value);
        let mut depends_on = Vec::new();

        for ref_name in &refs {
            if defined.contains(ref_name) {
                depends_on.push(ref_name.clone());
                edges.push(VariableEdge {
                    from: var.name.clone(),
                    to: ref_name.clone(),
                });
            } else {
                unresolved.push(UnresolvedReference {
                    referencing_variable: var.name.clone(),
                    referenced_name: ref_name.clone(),
                    line: var.line,
                });
            }
        }

        let idx = nodes.len();
        name_to_idx.insert(var.name.clone(), idx);

        nodes.push(VariableNode {
            name: var.name.clone(),
            value: var.value.clone(),
            line: var.line,
            scope: var.scope.clone(),
            depends_on,
            referenced_by: Vec::new(),
        });
    }

    // Fill referenced_by (reverse edges)
    for edge in &edges {
        if let Some(&idx) = name_to_idx.get(&edge.to) {
            nodes[idx].referenced_by.push(edge.from.clone());
        }
    }

    // Classify roots (no deps) and leaves (not referenced by anyone)
    let roots: Vec<String> = nodes
        .iter()
        .filter(|n| n.depends_on.is_empty())
        .map(|n| n.name.clone())
        .collect();

    let leaves: Vec<String> = nodes
        .iter()
        .filter(|n| n.referenced_by.is_empty())
        .map(|n| n.name.clone())
        .collect();

    // Cycle detection via DFS with 3-color marking
    let cycles = detect_cycles(&nodes, &name_to_idx);

    // Max depth via BFS from roots through referenced_by edges
    let max_depth = compute_max_depth(&nodes, &name_to_idx, &roots);

    let stats = VariableGraphStats {
        total_variables: nodes.len(),
        total_edges: edges.len(),
        root_count: roots.len(),
        leaf_count: leaves.len(),
        cycle_count: cycles.len(),
        unresolved_count: unresolved.len(),
        max_depth,
    };

    Ok(CssVariableGraph {
        file_path: file_path.to_string(),
        variables: nodes,
        edges,
        roots,
        leaves,
        cycles,
        unresolved,
        stats,
    })
}

/// Extract var(--name) references from a CSS value string.
/// Handles nested var() with fallbacks: var(--a, var(--b, red))
pub fn extract_var_references(value: &str) -> Vec<String> {
    let mut refs = Vec::new();
    let bytes = value.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i + 5 < len {
        // Look for "var("
        if &bytes[i..i + 4] == b"var(" {
            i += 4;
            // Skip whitespace
            while i < len && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            // Extract --name
            if i + 2 <= len && &bytes[i..i + 2] == b"--" {
                let start = i;
                while i < len
                    && (bytes[i].is_ascii_alphanumeric() || bytes[i] == b'-' || bytes[i] == b'_')
                {
                    i += 1;
                }
                let name = &value[start..i];
                if !name.is_empty() {
                    refs.push(name.to_string());
                }
            }
        } else {
            i += 1;
        }
    }

    refs
}

/// Detect cycles using DFS with 3-color marking (white=0, gray=1, black=2)
fn detect_cycles(
    nodes: &[VariableNode],
    name_to_idx: &HashMap<String, usize>,
) -> Vec<Vec<String>> {
    let n = nodes.len();
    let mut color = vec![0u8; n]; // 0=white, 1=gray, 2=black
    let mut path: Vec<usize> = Vec::new();
    let mut cycles: Vec<Vec<String>> = Vec::new();

    for start in 0..n {
        if color[start] == 0 {
            dfs_cycle(start, nodes, name_to_idx, &mut color, &mut path, &mut cycles);
        }
    }

    cycles
}

fn dfs_cycle(
    u: usize,
    nodes: &[VariableNode],
    name_to_idx: &HashMap<String, usize>,
    color: &mut [u8],
    path: &mut Vec<usize>,
    cycles: &mut Vec<Vec<String>>,
) {
    color[u] = 1; // gray
    path.push(u);

    for dep_name in &nodes[u].depends_on {
        if let Some(&v) = name_to_idx.get(dep_name) {
            if color[v] == 1 {
                // Found cycle — extract from path.
                // unwrap: color[v]==1 means v is currently on the DFS stack,
                // so v is guaranteed to be present in `path` by the traversal
                // invariant.
                let cycle_start = path.iter().position(|&x| x == v).unwrap();
                let cycle: Vec<String> = path[cycle_start..]
                    .iter()
                    .map(|&idx| nodes[idx].name.clone())
                    .collect();
                cycles.push(cycle);
            } else if color[v] == 0 {
                dfs_cycle(v, nodes, name_to_idx, color, path, cycles);
            }
        }
    }

    path.pop();
    color[u] = 2; // black
}

/// Compute maximum depth from roots through referenced_by edges (BFS)
fn compute_max_depth(
    nodes: &[VariableNode],
    name_to_idx: &HashMap<String, usize>,
    roots: &[String],
) -> usize {
    if roots.is_empty() || nodes.is_empty() {
        return 0;
    }

    let mut max_depth = 0usize;
    let mut visited = vec![false; nodes.len()];
    let mut queue: std::collections::VecDeque<(usize, usize)> = std::collections::VecDeque::new();

    for root in roots {
        if let Some(&idx) = name_to_idx.get(root) {
            if !visited[idx] {
                visited[idx] = true;
                queue.push_back((idx, 0));
            }
        }
    }

    while let Some((idx, depth)) = queue.pop_front() {
        if depth > max_depth {
            max_depth = depth;
        }
        for ref_name in &nodes[idx].referenced_by {
            if let Some(&ref_idx) = name_to_idx.get(ref_name) {
                if !visited[ref_idx] {
                    visited[ref_idx] = true;
                    queue.push_back((ref_idx, depth + 1));
                }
            }
        }
    }

    max_depth
}

// ========== Tests ==========

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn write_temp_css(name: &str, content: &str) -> String {
        let dir = std::env::temp_dir();
        let path = dir.join(name);
        let mut f = std::fs::File::create(&path).expect("create temp file");
        f.write_all(content.as_bytes()).expect("write temp file");
        path.to_str().unwrap().to_string()
    }

    #[test]
    fn test_basic_analysis() {
        let css = r#"
.header {
    color: red;
    font-size: 16px;
}

#main .content {
    background: blue;
    padding: 10px !important;
}
"#;
        let path = write_temp_css("test_basic.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 2);
        assert_eq!(result.stats.total_rules, 2);
        assert_eq!(result.stats.total_properties, 4);
        assert_eq!(result.stats.important_count, 1);

        // First rule
        assert_eq!(result.rules[0].selectors, vec![".header"]);
        assert_eq!(result.rules[0].properties.len(), 2);
        assert_eq!(result.rules[0].properties[0].name, "color");
        assert_eq!(result.rules[0].properties[0].value, "red");
        assert!(!result.rules[0].properties[0].is_important);

        // Second rule
        assert_eq!(result.rules[1].selectors, vec!["#main .content"]);
        assert!(result.rules[1].properties[1].is_important);
        assert_eq!(result.rules[1].properties[1].name, "padding");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_line_numbers() {
        let css = ".a { color: red; }\n\n.b {\n    font-size: 12px;\n}\n";
        let path = write_temp_css("test_lines.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 2);
        assert_eq!(result.rules[0].start_line, 1);
        assert_eq!(result.rules[0].end_line, 1);
        assert_eq!(result.rules[1].start_line, 3);
        assert_eq!(result.rules[1].end_line, 5);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_media_queries() {
        let css = r#"
.base { color: red; }

@media (max-width: 768px) {
    .mobile {
        display: none;
    }
    .responsive {
        padding: 8px;
    }
}
"#;
        let path = write_temp_css("test_media.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 1); // .base
        assert_eq!(result.media_queries.len(), 1);
        assert_eq!(result.media_queries[0].condition, "(max-width: 768px)");
        assert_eq!(result.media_queries[0].rules.len(), 2);
        assert!(result.media_queries[0].rules[0].is_nested);
        assert_eq!(
            result.media_queries[0].rules[0].parent_context,
            Some("@media (max-width: 768px)".to_string())
        );
        assert_eq!(result.stats.total_rules, 3); // 1 base + 2 nested

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_css_variables() {
        let css = r#"
:root {
    --color-primary: #3b82f6;
    --spacing-md: 16px;
}

.card {
    color: var(--color-primary);
    --card-bg: white;
}
"#;
        let path = write_temp_css("test_vars.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.variables.len(), 3);
        assert_eq!(result.variables[0].name, "--color-primary");
        assert_eq!(result.variables[0].value, "#3b82f6");
        assert_eq!(result.variables[0].scope, ":root");

        assert_eq!(result.variables[2].name, "--card-bg");
        assert_eq!(result.variables[2].scope, ".card");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_keyframes() {
        let css = r#"
@keyframes fadeIn {
    from { opacity: 0; }
    to { opacity: 1; }
}

.animate { animation: fadeIn 0.3s; }
"#;
        let path = write_temp_css("test_keyframes.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.keyframes.len(), 1);
        assert_eq!(result.keyframes[0].name, "fadeIn");
        assert_eq!(result.stats.total_keyframes, 1);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_comment_skipping() {
        let css = r#"
/* This is a comment */
.visible { color: red; }

/*
Multi-line
comment
*/

.also-visible {
    /* inline comment */
    font-size: 14px;
}
"#;
        let path = write_temp_css("test_comments.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 2);
        assert_eq!(result.rules[0].selectors, vec![".visible"]);
        assert_eq!(result.rules[1].selectors, vec![".also-visible"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_specificity_class() {
        let sp = compute_specificity(".foo");
        assert_eq!(sp.ids, 0);
        assert_eq!(sp.classes, 1);
        assert_eq!(sp.elements, 0);
        assert_eq!(sp.display, "0-1-0");
    }

    #[test]
    fn test_specificity_id_class() {
        let sp = compute_specificity("#bar .baz");
        assert_eq!(sp.ids, 1);
        assert_eq!(sp.classes, 1);
        assert_eq!(sp.display, "1-1-0");
    }

    #[test]
    fn test_specificity_complex() {
        let sp = compute_specificity("div.class#id");
        assert_eq!(sp.ids, 1);
        assert_eq!(sp.classes, 1);
        assert_eq!(sp.elements, 1);
        assert_eq!(sp.display, "1-1-1");
    }

    #[test]
    fn test_find_css_rules() {
        let css = r#"
.kb-sidebar { width: 300px; }
.kb-sidebar-header { padding: 8px; }
.kb-content { flex: 1; }
"#;
        let path = write_temp_css("test_find.css", css);
        let matches = find_css_rules(&path, "sidebar").unwrap();

        assert_eq!(matches.len(), 2);
        assert!(matches[0].selectors[0].contains("sidebar"));
        assert!(matches[1].selectors[0].contains("sidebar"));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_find_unused_selectors() {
        let css = r#"
.used-class { color: red; }
.unused-class { color: blue; }
.also-used { padding: 8px; }
"#;
        let component = r#"
export function App() {
    return <div className="used-class also-used">Hello</div>;
}
"#;
        let css_path = write_temp_css("test_unused.css", css);
        let comp_path = write_temp_css("test_component.tsx", component);

        let unused = find_unused_selectors(&css_path, &[comp_path.as_str()]).unwrap();

        assert_eq!(unused.len(), 1);
        assert_eq!(unused[0].selector, ".unused-class");

        let _ = std::fs::remove_file(css_path);
        let _ = std::fs::remove_file(comp_path);
    }

    #[test]
    fn test_multiple_selectors() {
        let css = ".a, .b, .c { color: red; }\n";
        let path = write_temp_css("test_multi_sel.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0].selectors, vec![".a", ".b", ".c"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_empty_file() {
        let path = write_temp_css("test_empty.css", "");
        let result = analyze_css(&path).unwrap();

        assert!(result.rules.is_empty());
        assert_eq!(result.stats.total_rules, 0);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_malformed_css_no_panic() {
        let css = "{{{{ color: red; } } }} .broken { font-size: }";
        let path = write_temp_css("test_malformed.css", css);
        let result = analyze_css(&path);
        // Should not panic, result is Ok
        assert!(result.is_ok());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_font_face() {
        let css = r#"
@font-face {
    font-family: 'CustomFont';
    src: url('font.woff2');
}
"#;
        let path = write_temp_css("test_fontface.css", css);
        let result = analyze_css(&path).unwrap();

        assert_eq!(result.rules.len(), 1);
        assert_eq!(result.rules[0].selectors, vec!["@font-face"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_extract_class_names() {
        assert_eq!(extract_class_names(".foo"), vec!["foo"]);
        assert_eq!(extract_class_names(".foo .bar"), vec!["foo", "bar"]);
        assert_eq!(extract_class_names("#id .cls"), vec!["cls"]);
        assert_eq!(extract_class_names("div.my-class"), vec!["my-class"]);
        assert!(extract_class_names("div").is_empty());
    }

    // ===== Variable dependency graph tests =====

    #[test]
    fn test_extract_var_refs_single() {
        let refs = extract_var_references("var(--color-primary)");
        assert_eq!(refs, vec!["--color-primary"]);
    }

    #[test]
    fn test_extract_var_refs_multiple() {
        let refs = extract_var_references("linear-gradient(var(--from), var(--to))");
        assert_eq!(refs, vec!["--from", "--to"]);
    }

    #[test]
    fn test_extract_var_refs_with_fallback() {
        let refs = extract_var_references("var(--color-bg, var(--fallback-bg, white))");
        assert_eq!(refs, vec!["--color-bg", "--fallback-bg"]);
    }

    #[test]
    fn test_extract_var_refs_nested() {
        let refs = extract_var_references("var(--a, var(--b))");
        assert_eq!(refs, vec!["--a", "--b"]);
    }

    #[test]
    fn test_extract_var_refs_none() {
        let refs = extract_var_references("#3b82f6");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_extract_var_refs_plain_value() {
        let refs = extract_var_references("16px");
        assert!(refs.is_empty());
    }

    #[test]
    fn test_variable_graph_basic_chain() {
        let css = r#"
:root {
    --base: blue;
    --primary: var(--base);
    --btn-bg: var(--primary);
}
"#;
        let path = write_temp_css("test_vargraph_chain.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.total_variables, 3);
        assert_eq!(graph.stats.total_edges, 2);

        // --base is a root (no deps)
        assert!(graph.roots.contains(&"--base".to_string()));
        // --btn-bg is a leaf (not referenced by anyone)
        assert!(graph.leaves.contains(&"--btn-bg".to_string()));

        // Check edges
        let has_edge = |from: &str, to: &str| {
            graph.edges.iter().any(|e| e.from == from && e.to == to)
        };
        assert!(has_edge("--primary", "--base"));
        assert!(has_edge("--btn-bg", "--primary"));

        assert_eq!(graph.stats.cycle_count, 0);
        assert_eq!(graph.stats.max_depth, 2);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_roots_and_leaves() {
        let css = r#"
:root {
    --a: red;
    --b: green;
    --c: var(--a);
}
"#;
        let path = write_temp_css("test_vargraph_roots.css", css);
        let graph = css_variable_graph(&path).unwrap();

        // --a and --b are roots (no deps)
        assert!(graph.roots.contains(&"--a".to_string()));
        assert!(graph.roots.contains(&"--b".to_string()));
        // --b and --c are leaves (not referenced)
        assert!(graph.leaves.contains(&"--b".to_string()));
        assert!(graph.leaves.contains(&"--c".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_cycle_detection() {
        let css = r#"
:root {
    --a: var(--b);
    --b: var(--c);
    --c: var(--a);
}
"#;
        let path = write_temp_css("test_vargraph_cycle.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert!(graph.stats.cycle_count > 0);
        // The cycle should contain --a, --b, --c
        let cycle = &graph.cycles[0];
        assert!(cycle.contains(&"--a".to_string()));
        assert!(cycle.contains(&"--b".to_string()));
        assert!(cycle.contains(&"--c".to_string()));

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_self_cycle() {
        let css = ":root {\n    --loop: var(--loop);\n}\n";
        let path = write_temp_css("test_vargraph_selfcycle.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert!(graph.stats.cycle_count > 0);
        assert_eq!(graph.cycles[0], vec!["--loop"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_unresolved() {
        let css = r#"
:root {
    --defined: blue;
    --uses-missing: var(--not-defined);
}
"#;
        let path = write_temp_css("test_vargraph_unresolved.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.unresolved_count, 1);
        assert_eq!(graph.unresolved[0].referenced_name, "--not-defined");
        assert_eq!(graph.unresolved[0].referencing_variable, "--uses-missing");

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_max_depth() {
        let css = r#"
:root {
    --l0: red;
    --l1: var(--l0);
    --l2: var(--l1);
    --l3: var(--l2);
}
"#;
        let path = write_temp_css("test_vargraph_depth.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.max_depth, 3);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_multiple_refs() {
        let css = r#"
:root {
    --a: red;
    --b: green;
    --c: var(--a) var(--b);
}
"#;
        let path = write_temp_css("test_vargraph_multirefs.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.total_edges, 2);
        let c_node = graph.variables.iter().find(|n| n.name == "--c").unwrap();
        assert_eq!(c_node.depends_on.len(), 2);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_scoped_vars() {
        let css = r#"
:root {
    --global: blue;
}
.card {
    --card-bg: var(--global);
}
"#;
        let path = write_temp_css("test_vargraph_scoped.css", css);
        let graph = css_variable_graph(&path).unwrap();

        let card_node = graph.variables.iter().find(|n| n.name == "--card-bg").unwrap();
        assert_eq!(card_node.scope, ".card");
        assert_eq!(card_node.depends_on, vec!["--global"]);

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_no_variables() {
        let css = ".foo { color: red; }\n";
        let path = write_temp_css("test_vargraph_novars.css", css);
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.total_variables, 0);
        assert!(graph.variables.is_empty());
        assert!(graph.edges.is_empty());

        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn test_variable_graph_empty_file() {
        let path = write_temp_css("test_vargraph_empty.css", "");
        let graph = css_variable_graph(&path).unwrap();

        assert_eq!(graph.stats.total_variables, 0);
        assert_eq!(graph.stats.max_depth, 0);

        let _ = std::fs::remove_file(path);
    }
}
