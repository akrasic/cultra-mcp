use serde::{Deserialize, Serialize};
use crate::mcp::types::{Language, Scope, SymbolType};

/// Symbol represents a code symbol (function, type, class, etc.)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Symbol {
    #[serde(rename = "type")]
    pub symbol_type: SymbolType,

    pub name: String, // Symbol name

    pub line: u32, // Starting line number

    pub end_line: u32, // Ending line number

    pub scope: Scope,

    pub signature: String, // Function signature or type definition (concise)

    #[serde(skip_serializing_if = "Option::is_none")]
    pub parent: Option<String>, // Parent type/class if method

    #[serde(skip_serializing_if = "Option::is_none")]
    pub receiver: Option<String>, // Method receiver (Go only, e.g., "*DB")

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub calls: Vec<String>, // Functions/methods this symbol calls

    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>, // Doc comments, docstrings

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parameters: Vec<Param>, // Function parameters with types

    #[serde(skip_serializing_if = "Option::is_none")]
    pub return_type: Option<String>, // Return type
}

/// Param represents a function parameter
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Param {
    pub name: String,

    #[serde(rename = "type")]
    pub param_type: String,
}

/// FileContext represents AST metadata for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileContext {
    pub file_path: String, // Absolute file path

    pub language: Language,

    pub symbols: Vec<Symbol>, // Extracted symbols

    pub imports: Vec<String>, // Import statements

    pub ast_stats: ASTStats, // AST complexity metrics
}

/// ASTStats provides metrics about the AST
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ASTStats {
    pub total_nodes: usize, // Total AST nodes

    pub max_depth: usize, // Maximum tree depth
}

/// TypeInfo represents detailed type information (for Go structs/interfaces)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TypeInfo {
    pub name: String,

    pub kind: String, // "struct", "interface", "alias", "primitive"

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub fields: Vec<FieldInfo>,

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub methods: Vec<String>, // Method names on this type

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub implements: Vec<String>, // Interfaces this type implements

    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub embedded: Vec<String>, // Embedded types (Go)

    pub location: String,
}

/// FieldInfo represents a struct field
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldInfo {
    pub name: String,

    #[serde(rename = "type")]
    pub field_type: String,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tag: Option<String>, // Struct tag (Go)
}

impl Symbol {
    /// Helper to create a new Symbol with defaults
    pub fn new(symbol_type: SymbolType, name: String, line: u32, end_line: u32) -> Self {
        Self {
            symbol_type,
            name,
            line,
            end_line,
            scope: Scope::Public,
            signature: String::new(),
            parent: None,
            receiver: None,
            calls: Vec::new(),
            documentation: None,
            parameters: Vec::new(),
            return_type: None,
        }
    }

    /// Format location as "file:line" or "file:start-end"
    pub fn location(&self, file_path: &str) -> String {
        if self.line == self.end_line {
            format!("{}:{}", file_path, self.line)
        } else {
            format!("{}:{}-{}", file_path, self.line, self.end_line)
        }
    }
}

impl FileContext {
    /// Create a new empty FileContext
    pub fn new(file_path: String, language: Language) -> Self {
        Self {
            file_path,
            language,
            symbols: Vec::new(),
            imports: Vec::new(),
            ast_stats: ASTStats {
                total_nodes: 0,
                max_depth: 0,
            },
        }
    }
}
