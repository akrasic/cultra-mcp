use super::types::{FileContext, Symbol, ASTStats};
use super::util::{detect_language, calculate_ast_stats};
use super::languages;
use crate::mcp::types::{Language as LangEnum, SymbolType};
use anyhow::{Context, Result};
use std::fs;
use tree_sitter::Parser as TSParser;

/// AST Parser for multiple programming languages
pub struct Parser {
    // Parser can be reused across multiple files
}

impl Parser {
    /// Create a new AST parser
    pub fn new() -> Self {
        Self {}
    }

    /// Parse a file and extract AST metadata
    pub fn parse_file(&self, file_path: &str) -> Result<FileContext> {
        // 1. Read file content
        let content = fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read file: {}", file_path))?;

        // 2. Detect language from extension
        let language = detect_language(file_path)
            .ok_or_else(|| anyhow::anyhow!("Unsupported file type: {}", file_path))?;

        // Svelte: extract <script> block and parse as TypeScript
        if language == "svelte" {
            return self.parse_svelte_file(file_path, &content);
        }

        // 3. Create tree-sitter parser
        let mut ts_parser = TSParser::new();

        // Set language-specific parser
        let ts_language = self.get_tree_sitter_language(language)?;
        ts_parser
            .set_language(&ts_language)
            .with_context(|| format!("Failed to set language: {}", language))?;

        // 4. Parse source code
        let tree = ts_parser
            .parse(&content, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse file"))?;

        let root_node = tree.root_node();

        // 5. Extract symbols based on language
        let symbols = self.extract_symbols(language, &root_node, content.as_bytes())?;

        // 6. Extract imports
        let imports = self.extract_imports(language, &root_node, content.as_bytes());

        // 7. Calculate AST statistics
        let (total_nodes, max_depth) = calculate_ast_stats(&root_node);
        let ast_stats = ASTStats {
            total_nodes,
            max_depth,
        };

        Ok(FileContext {
            file_path: file_path.to_string(),
            language: LangEnum::from_str(language),
            symbols,
            imports,
            ast_stats,
        })
    }

    /// Parse a Svelte file by extracting <script> block and parsing as TypeScript
    fn parse_svelte_file(&self, file_path: &str, content: &str) -> Result<FileContext> {
        let (script_content, line_offset) = extract_svelte_script(content);

        if script_content.is_empty() {
            return Ok(FileContext {
                file_path: file_path.to_string(),
                language: LangEnum::Svelte,
                symbols: Vec::new(),
                imports: Vec::new(),
                ast_stats: ASTStats {
                    total_nodes: 0,
                    max_depth: 0,
                },
            });
        }

        // Parse the script content as TypeScript
        let mut ts_parser = TSParser::new();
        let ts_language: tree_sitter::Language =
            tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        ts_parser
            .set_language(&ts_language)
            .with_context(|| "Failed to set TypeScript language for Svelte")?;

        let tree = ts_parser
            .parse(&script_content, None)
            .ok_or_else(|| anyhow::anyhow!("Failed to parse Svelte script block"))?;

        let root_node = tree.root_node();

        // Extract symbols and imports using TypeScript extractors.
        // Svelte <script lang="ts"> is parsed with the TS grammar.
        let ts_lang: tree_sitter::Language = tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into();
        let mut symbols =
            languages::extract_typescript_symbols(&root_node, script_content.as_bytes(), &ts_lang)?;
        let imports = languages::typescript::extract_typescript_imports(
            &root_node,
            script_content.as_bytes(),
            &ts_lang,
        )
        .unwrap_or_default();

        // Offset line numbers to match the original .svelte file
        for symbol in &mut symbols {
            symbol.line += line_offset;
            symbol.end_line += line_offset;
        }

        // CULTRA-973: extract symbols from the template section (outside <script>/<style>).
        // This catches @const declarations, inline event handlers, and component references
        // that the script-only parse misses in template-heavy Svelte components.
        symbols.extend(extract_svelte_template_symbols(content));

        let (total_nodes, max_depth) = calculate_ast_stats(&root_node);

        Ok(FileContext {
            file_path: file_path.to_string(),
            language: LangEnum::Svelte,
            symbols,
            imports,
            ast_stats: ASTStats {
                total_nodes,
                max_depth,
            },
        })
    }

    /// Get tree-sitter Language for a given language name
    fn get_tree_sitter_language(&self, language: &str) -> Result<tree_sitter::Language> {
        match language {
            "go" => Ok(tree_sitter_go::LANGUAGE.into()),
            "typescript" => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            "tsx" => Ok(tree_sitter_typescript::LANGUAGE_TSX.into()),
            "javascript" => Ok(tree_sitter_javascript::LANGUAGE.into()),
            "python" => Ok(tree_sitter_python::LANGUAGE.into()),
            "rust" => Ok(tree_sitter_rust::LANGUAGE.into()),
            "terraform" => Ok(tree_sitter_hcl::LANGUAGE.into()),
            _ => Err(anyhow::anyhow!("Unsupported language: {}", language)),
        }
    }

    /// Extract symbols based on language
    fn extract_symbols(
        &self,
        language: &str,
        root_node: &tree_sitter::Node,
        content: &[u8],
    ) -> Result<Vec<Symbol>> {
        match language {
            "go" => languages::extract_go_symbols(root_node, content),
            "typescript" | "tsx" | "javascript" => {
                let ts_lang = self.get_tree_sitter_language(language)?;
                languages::extract_typescript_symbols(root_node, content, &ts_lang)
            }
            "python" => languages::extract_python_symbols(root_node, content),
            "rust" => languages::extract_rust_symbols(root_node, content),
            "terraform" => languages::extract_terraform_symbols(root_node, content),
            _ => Ok(Vec::new()),
        }
    }

    /// Extract imports based on language
    fn extract_imports(
        &self,
        language: &str,
        root_node: &tree_sitter::Node,
        content: &[u8],
    ) -> Vec<String> {
        match language {
            "go" => languages::go::extract_go_imports(root_node, content).unwrap_or_default(),
            "rust" => {
                languages::rust_lang::extract_rust_imports(root_node, content).unwrap_or_default()
            }
            "typescript" | "tsx" | "javascript" => {
                let ts_lang = match self.get_tree_sitter_language(language) {
                    Ok(l) => l,
                    Err(_) => return Vec::new(),
                };
                languages::typescript::extract_typescript_imports(root_node, content, &ts_lang)
                    .unwrap_or_default()
            }
            "python" => {
                languages::python::extract_python_imports(root_node, content).unwrap_or_default()
            }
            "terraform" => {
                languages::terraform::extract_terraform_imports(root_node, content).unwrap_or_default()
            }
            _ => Vec::new(),
        }
    }
}

impl Default for Parser {
    fn default() -> Self {
        Self::new()
    }
}

/// Extract the content of the first `<script>` block from a Svelte file.
/// Returns (script_content, line_offset) where line_offset is the 0-based
/// line number of the `<script>` opening tag (so symbol lines can be adjusted).
pub fn extract_svelte_script(content: &str) -> (String, u32) {
    // Find <script ...> tag (with optional attributes like lang="ts")
    let script_open_re = regex_find_script_open(content);
    if let Some((open_end_byte, open_line)) = script_open_re {
        // Find matching </script>
        if let Some(close_start) = content[open_end_byte..].find("</script>") {
            let script_body = &content[open_end_byte..open_end_byte + close_start];
            return (script_body.to_string(), open_line as u32);
        }
    }
    (String::new(), 0)
}

/// CULTRA-973: Extract symbols from Svelte template sections.
///
/// Scans lines outside `<script>`/`<style>` blocks for:
/// - `{@const name = ...}` → Variable symbol
/// - `on:event={handler}` / `onevent={...}` → inline event handler symbols
/// - `<ComponentName` (uppercase) → component reference symbols
///
/// These are lightweight regex scans — no tree-sitter grammar needed.
fn extract_svelte_template_symbols(content: &str) -> Vec<Symbol> {
    use crate::mcp::types::{Scope, SymbolType};

    let mut symbols = Vec::new();
    let mut in_script = false;
    let mut in_style = false;

    for (line_idx, line) in content.lines().enumerate() {
        let trimmed = line.trim();
        let line_num = (line_idx + 1) as u32;

        // Track script/style blocks to skip them
        if trimmed.starts_with("<script") {
            in_script = true;
            continue;
        }
        if trimmed.starts_with("</script") {
            in_script = false;
            continue;
        }
        if trimmed.starts_with("<style") {
            in_style = true;
            continue;
        }
        if trimmed.starts_with("</style") {
            in_style = false;
            continue;
        }
        if in_script || in_style {
            continue;
        }

        // {@const name = expression}
        if let Some(rest) = trimmed.strip_prefix("{@const ") {
            if let Some(eq_pos) = rest.find('=') {
                let name = rest[..eq_pos].trim().to_string();
                if !name.is_empty() && name.chars().next().map_or(false, |c| c.is_alphabetic() || c == '_') {
                    symbols.push(Symbol {
                        symbol_type: SymbolType::Variable,
                        name,
                        line: line_num,
                        end_line: line_num,
                        scope: Scope::Private,
                        signature: trimmed.trim_end_matches('}').to_string(),
                        parent: None,
                        receiver: None,
                        calls: Vec::new(),
                        documentation: None,
                        parameters: Vec::new(),
                        return_type: None,
                    });
                }
            }
        }

        // Inline event handlers: onclick={...} or on:click={...}
        // Extract handler name or arrow function
        let line_str = line;
        let mut search_from = 0;
        while search_from < line_str.len() {
            // Match onclick=, on:click=, onchange=, etc.
            let handler_start = line_str[search_from..]
                .find("on")
                .map(|p| p + search_from);
            let handler_start = match handler_start {
                Some(pos) => pos,
                None => break,
            };

            let after_on = &line_str[handler_start + 2..];
            // Must be followed by a letter (onclick) or colon (on:click)
            let is_event = after_on.starts_with(':')
                || after_on.chars().next().map_or(false, |c| c.is_ascii_lowercase());
            if !is_event {
                search_from = handler_start + 2;
                continue;
            }

            // Find the = sign and opening {
            if let Some(eq_rel) = after_on.find("={") {
                let event_name_end = handler_start + 2 + eq_rel;
                let event_name = &line_str[handler_start..event_name_end];

                // Extract the handler body between { and }
                let brace_start = handler_start + 2 + eq_rel + 1; // position of {
                if let Some(body) = extract_balanced_braces(&line_str[brace_start..]) {
                    let body_trimmed = body.trim();
                    // Determine handler name: if it's a simple reference like {handleClick},
                    // use that. If it's an arrow () => ..., name it as event handler.
                    let handler_name = if !body_trimmed.contains("=>") && !body_trimmed.contains('(') {
                        body_trimmed.to_string()
                    } else {
                        format!("{}:handler", event_name)
                    };

                    // Only emit if it's an arrow function (simple refs are already in script symbols)
                    if body_trimmed.contains("=>") {
                        symbols.push(Symbol {
                            symbol_type: SymbolType::Function,
                            name: handler_name,
                            line: line_num,
                            end_line: line_num,
                            scope: Scope::Private,
                            signature: format!("{}={{...}}", event_name),
                            parent: None,
                            receiver: None,
                            calls: Vec::new(),
                            documentation: None,
                            parameters: Vec::new(),
                            return_type: None,
                        });
                    }
                }
                search_from = event_name_end + 2;
            } else {
                search_from = handler_start + 2;
            }
        }

        // Component references: <ComponentName (starts with uppercase)
        if let Some(lt_pos) = trimmed.find('<') {
            let after_lt = &trimmed[lt_pos + 1..];
            if after_lt.starts_with(|c: char| c.is_ascii_uppercase()) {
                let comp_end = after_lt.find(|c: char| !c.is_alphanumeric() && c != '_' && c != '.')
                    .unwrap_or(after_lt.len());
                let comp_name = &after_lt[..comp_end];
                // Skip HTML-like tags (SVG, DOCTYPE, etc.)
                if comp_name.len() > 1 && !comp_name.chars().all(|c| c.is_ascii_uppercase()) {
                    symbols.push(Symbol {
                        symbol_type: SymbolType::Variable, // component reference
                        name: comp_name.to_string(),
                        line: line_num,
                        end_line: line_num,
                        scope: Scope::Public,
                        signature: format!("<{} />", comp_name),
                        parent: None,
                        receiver: None,
                        calls: Vec::new(),
                        documentation: None,
                        parameters: Vec::new(),
                        return_type: None,
                    });
                }
            }
        }
    }

    // Deduplicate component references (same component used multiple times)
    let mut seen_components = std::collections::HashSet::new();
    symbols.retain(|s| {
        if s.signature.starts_with('<') {
            seen_components.insert(s.name.clone())
        } else {
            true
        }
    });

    symbols
}

/// Extract content between balanced braces. Returns the content without
/// the outer braces, or None if braces aren't balanced on this line.
fn extract_balanced_braces(s: &str) -> Option<&str> {
    if !s.starts_with('{') {
        return None;
    }
    let mut depth = 0;
    for (i, c) in s.char_indices() {
        match c {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&s[1..i]);
                }
            }
            _ => {}
        }
    }
    None
}

/// Find the end position and line number of the first <script...> opening tag.
fn regex_find_script_open(content: &str) -> Option<(usize, usize)> {
    let bytes = content.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'<' {
            let rest = &content[i..];
            // Match <script or <script followed by space/>
            if rest.len() > 7 && rest[1..7].eq_ignore_ascii_case("script") {
                let tag_char = rest.as_bytes().get(7).copied();
                if tag_char == Some(b'>') || tag_char == Some(b' ') || tag_char == Some(b'\n') {
                    // Find the closing >
                    if let Some(gt_pos) = rest.find('>') {
                        let end_byte = i + gt_pos + 1;
                        let line = content[..end_byte].matches('\n').count();
                        return Some((end_byte, line));
                    }
                }
            }
        }
        i += 1;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_tree_sitter_language() {
        let parser = Parser::new();

        assert!(parser.get_tree_sitter_language("go").is_ok());
        assert!(parser.get_tree_sitter_language("typescript").is_ok());
        assert!(parser.get_tree_sitter_language("javascript").is_ok());
        assert!(parser.get_tree_sitter_language("python").is_ok());
        assert!(parser.get_tree_sitter_language("rust").is_ok());
        assert!(parser.get_tree_sitter_language("unknown").is_err());
    }

    #[test]
    fn test_parse_rust_file() {
        use std::io::Write;

        let source = r#"
use std::fmt;

pub struct Calculator {
    value: i32,
}

impl Calculator {
    pub fn new() -> Self {
        Self { value: 0 }
    }
}

pub trait Compute {
    fn compute(&self) -> i32;
}

fn main() {
    let calc = Calculator::new();
}
"#;

        // Write to temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_parser_rust.rs");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(source.as_bytes()).expect("Failed to write temp file");

        // Parse the file
        let parser = Parser::new();
        let result = parser.parse_file(test_file.to_str().unwrap());

        assert!(result.is_ok(), "Failed to parse Rust file");
        let file_context = result.unwrap();

        assert_eq!(file_context.language, LangEnum::Rust);
        assert!(file_context.symbols.len() >= 3, "Should extract at least 3 symbols (struct, trait, impl)");

        let has_struct = file_context.symbols.iter().any(|s| s.name == "Calculator" && s.symbol_type == SymbolType::Struct);
        let has_trait = file_context.symbols.iter().any(|s| s.name == "Compute" && s.symbol_type == SymbolType::Interface);
        let has_main = file_context.symbols.iter().any(|s| s.name == "main" && s.symbol_type == SymbolType::Function);

        assert!(has_struct, "Should find Calculator struct");
        assert!(has_trait, "Should find Compute trait");
        assert!(has_main, "Should find main function");

        let has_std_fmt = file_context.imports.iter().any(|i| i.contains("std::fmt"));
        assert!(has_std_fmt, "Should find `use std::fmt` import, got: {:?}", file_context.imports);

        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_parse_typescript_file() {
        use std::io::Write;

        let source = r#"
import { User } from './types';

interface Config {
    port: number;
}

class Server {
    start(): void {
        console.log("Starting");
    }
}

export function createServer(config: Config): Server {
    return new Server();
}
"#;

        // Write to temp file
        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_parser_typescript.ts");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(source.as_bytes())
            .expect("Failed to write temp file");

        // Parse the file
        let parser = Parser::new();
        let result = parser.parse_file(test_file.to_str().unwrap());

        assert!(result.is_ok(), "Failed to parse TypeScript file");
        let file_context = result.unwrap();

        // Verify results
        assert_eq!(file_context.language, LangEnum::Typescript);
        assert!(
            file_context.symbols.len() >= 3,
            "Should extract at least 3 symbols (interface, class, function)"
        );
        assert!(
            file_context.imports.len() >= 1,
            "Should extract at least 1 import"
        );

        // Check for specific symbols
        let has_interface = file_context
            .symbols
            .iter()
            .any(|s| s.name == "Config" && s.symbol_type == SymbolType::Interface);
        let has_class = file_context
            .symbols
            .iter()
            .any(|s| s.name == "Server" && s.symbol_type == SymbolType::Class);
        let has_function = file_context
            .symbols
            .iter()
            .any(|s| s.name == "createServer" && s.symbol_type == SymbolType::Function);

        assert!(has_interface, "Should find Config interface");
        assert!(has_class, "Should find Server class");
        assert!(has_function, "Should find createServer function");

        // Clean up
        let _ = std::fs::remove_file(test_file);
    }

    #[test]
    fn test_extract_svelte_script_basic() {
        let content = r#"<script lang="ts">
  import { onMount } from 'svelte';

  let count = 0;
  function increment() {
    count += 1;
  }
</script>

<button on:click={increment}>{count}</button>

<style>
  button { color: red; }
</style>"#;

        let (script, offset) = extract_svelte_script(content);
        assert!(!script.is_empty(), "Should extract script content");
        assert!(script.contains("import { onMount }"), "Should contain import");
        assert!(script.contains("function increment"), "Should contain function");
        assert!(!script.contains("<button"), "Should not contain template");
        assert!(!script.contains("<style>"), "Should not contain style");
        // Offset = number of newlines before end of <script> tag = 0 (tag is on line 0)
        assert_eq!(offset, 0, "Script tag is on line 0");
    }

    #[test]
    fn test_extract_svelte_script_no_script() {
        let content = "<div>Just a template</div>";
        let (script, _) = extract_svelte_script(content);
        assert!(script.is_empty());
    }

    #[test]
    fn test_extract_svelte_script_plain() {
        let content = "<script>\nlet x = 1;\n</script>";
        let (script, offset) = extract_svelte_script(content);
        assert!(script.contains("let x = 1;"));
        assert_eq!(offset, 0);
    }

    #[test]
    fn test_parse_svelte_file() {
        use std::io::Write;

        let source = r#"<script lang="ts">
  import { api } from '$lib/api';

  interface Props {
    title: string;
  }

  let { title }: Props = $props();

  function handleClick() {
    console.log(title);
  }
</script>

<h1>{title}</h1>
<button on:click={handleClick}>Click</button>
"#;

        let temp_dir = std::env::temp_dir();
        let test_file = temp_dir.join("test_parser_svelte.svelte");
        let mut file = std::fs::File::create(&test_file).expect("Failed to create temp file");
        file.write_all(source.as_bytes()).expect("Failed to write temp file");

        let parser = Parser::new();
        let result = parser.parse_file(test_file.to_str().unwrap());

        assert!(result.is_ok(), "Failed to parse Svelte file: {:?}", result.err());
        let ctx = result.unwrap();

        assert_eq!(ctx.language, LangEnum::Svelte);
        assert!(!ctx.symbols.is_empty(), "Should extract symbols from script block");
        assert!(!ctx.imports.is_empty(), "Should extract imports from script block");

        // Check that line numbers are offset (not starting at 1)
        let func = ctx.symbols.iter().find(|s| s.name == "handleClick");
        assert!(func.is_some(), "Should find handleClick function");
        let func = func.unwrap();
        assert!(func.line > 1, "Line number should be offset from script tag, got {}", func.line);

        let has_interface = ctx.symbols.iter().any(|s| s.name == "Props" && s.symbol_type == SymbolType::Interface);
        assert!(has_interface, "Should find Props interface");

        let _ = std::fs::remove_file(test_file);
    }
}
