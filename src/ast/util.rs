use std::path::Path;

/// Detect programming language from file extension
pub fn detect_language(file_path: &str) -> Option<&'static str> {
    let path = Path::new(file_path);
    let extension = path.extension()?.to_str()?;

    match extension {
        "go" => Some("go"),
        "ts" => Some("typescript"),
        "tsx" => Some("tsx"),
        "js" => Some("javascript"),
        "jsx" => Some("javascript"),
        "py" => Some("python"),
        "rs" => Some("rust"),
        "tf" | "tfvars" => Some("terraform"),
        "svelte" => Some("svelte"),
        _ => None,
    }
}

/// Calculate AST statistics by traversing tree
pub fn calculate_ast_stats(node: &tree_sitter::Node) -> (usize, usize) {
    let mut total_nodes = 0;
    let max_depth = calculate_depth_recursive(node, &mut total_nodes);
    (total_nodes, max_depth)
}

fn calculate_depth_recursive(node: &tree_sitter::Node, total_nodes: &mut usize) -> usize {
    *total_nodes += 1;

    let mut max_child_depth = 0;
    let mut cursor = node.walk();

    for child in node.children(&mut cursor) {
        let child_depth = calculate_depth_recursive(&child, total_nodes);
        max_child_depth = max_child_depth.max(child_depth);
    }

    1 + max_child_depth
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("main.go"), Some("go"));
        assert_eq!(detect_language("app.ts"), Some("typescript"));
        assert_eq!(detect_language("component.tsx"), Some("tsx"));
        assert_eq!(detect_language("script.js"), Some("javascript"));
        assert_eq!(detect_language("module.py"), Some("python"));
        assert_eq!(detect_language("lib.rs"), Some("rust"));
        assert_eq!(detect_language("main.tf"), Some("terraform"));
        assert_eq!(detect_language("vars.tfvars"), Some("terraform"));
        assert_eq!(detect_language("unknown.txt"), None);
    }
}
