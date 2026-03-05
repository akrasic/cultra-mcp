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

    /// Get tree-sitter Language for a given language name
    fn get_tree_sitter_language(&self, language: &str) -> Result<tree_sitter::Language> {
        match language {
            "go" => Ok(tree_sitter_go::LANGUAGE.into()),
            "typescript" => Ok(tree_sitter_typescript::LANGUAGE_TYPESCRIPT.into()),
            "tsx" => Ok(tree_sitter_typescript::LANGUAGE_TSX.into()),
            "javascript" => Ok(tree_sitter_javascript::LANGUAGE.into()),
            "python" => Ok(tree_sitter_python::LANGUAGE.into()),
            "rust" => Ok(tree_sitter_rust::LANGUAGE.into()),
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
                languages::extract_typescript_symbols(root_node, content)
            }
            "python" => languages::extract_python_symbols(root_node, content),
            "rust" => languages::extract_rust_symbols(root_node, content),
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
                languages::typescript::extract_typescript_imports(root_node, content)
                    .unwrap_or_default()
            }
            "python" => {
                languages::python::extract_python_imports(root_node, content).unwrap_or_default()
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parser_creation() {
        let _parser = Parser::new();
        // Parser created successfully
    }

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

        // Verify results
        assert_eq!(file_context.language, LangEnum::Rust);
        assert!(file_context.symbols.len() >= 3, "Should extract at least 3 symbols (struct, trait, impl)");
        assert!(file_context.imports.len() >= 1, "Should extract at least 1 import");

        // Check for specific symbols
        let has_struct = file_context.symbols.iter().any(|s| s.name == "Calculator" && s.symbol_type == SymbolType::Struct);
        let has_trait = file_context.symbols.iter().any(|s| s.name == "Compute" && s.symbol_type == SymbolType::Interface);

        assert!(has_struct, "Should find Calculator struct");
        assert!(has_trait, "Should find Compute trait");

        // Clean up
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
}
