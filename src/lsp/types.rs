// LSP Protocol Types
//
// Core types from the Language Server Protocol specification.
// See: https://microsoft.github.io/language-server-protocol/specification

use serde::{Deserialize, Deserializer, Serialize};

// ============================================================================
// LSP Capability Helper
// ============================================================================

/// Deserializes an LSP capability field that can be either a boolean or an options object.
///
/// Per LSP 3.17 spec, many capability fields are union types like `boolean | DefinitionOptions`.
/// - `true` / `false` → simple boolean
/// - `{"workDoneProgress": true, ...}` → object means capability is supported
/// - absent (None) → capability not supported
///
/// Pyright uses the object form; gopls/rust-analyzer use the boolean form.
fn deserialize_capability<'de, D>(deserializer: D) -> Result<Option<bool>, D::Error>
where
    D: Deserializer<'de>,
{
    let value: Option<serde_json::Value> = Option::deserialize(deserializer)?;
    match value {
        None => Ok(None),
        Some(serde_json::Value::Bool(b)) => Ok(Some(b)),
        Some(serde_json::Value::Object(_)) => Ok(Some(true)), // Any object = supported
        Some(other) => Err(serde::de::Error::custom(format!(
            "expected bool or object for capability, got: {}",
            other
        ))),
    }
}

// ============================================================================
// Core Position Types
// ============================================================================

/// Position in a text document (0-indexed)
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Position {
    /// Line number (0-indexed)
    pub line: u32,
    /// Character offset (0-indexed, UTF-16 code units)
    pub character: u32,
}

/// Range in a text document
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Range {
    pub start: Position,
    pub end: Position,
}

/// Location in a document (file + range)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Location {
    pub uri: String,  // file:// URI
    pub range: Range,
}

// ============================================================================
// Text Document Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentIdentifier {
    pub uri: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TextDocumentPositionParams {
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
}

// ============================================================================
// Initialize Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeParams {
    /// Process ID of the parent process (null if no parent)
    #[serde(rename = "processId")]
    pub process_id: Option<u32>,

    /// Root URI of the workspace
    #[serde(rename = "rootUri")]
    pub root_uri: Option<String>,

    /// Client capabilities
    pub capabilities: ClientCapabilities,

    /// Workspace folders (pyright requires this for workspace/symbol queries)
    #[serde(skip_serializing_if = "Option::is_none", rename = "workspaceFolders")]
    pub workspace_folders: Option<Vec<WorkspaceFolder>>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceFolder {
    pub uri: String,
    pub name: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ClientCapabilities {
    // Simplified - can expand as needed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace: Option<serde_json::Value>,

    #[serde(skip_serializing_if = "Option::is_none", rename = "textDocument")]
    pub text_document: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InitializeResult {
    pub capabilities: ServerCapabilities,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ServerCapabilities {
    // Each capability can be `true`, `false`, or an options object like `{"workDoneProgress": true}`.
    // We use a custom deserializer to normalize both forms to Option<bool>.
    #[serde(default, skip_serializing_if = "Option::is_none", rename = "definitionProvider", deserialize_with = "deserialize_capability")]
    pub definition_provider: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "referencesProvider", deserialize_with = "deserialize_capability")]
    pub references_provider: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "hoverProvider", deserialize_with = "deserialize_capability")]
    pub hover_provider: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "documentSymbolProvider", deserialize_with = "deserialize_capability")]
    pub document_symbol_provider: Option<bool>,

    #[serde(default, skip_serializing_if = "Option::is_none", rename = "workspaceSymbolProvider", deserialize_with = "deserialize_capability")]
    pub workspace_symbol_provider: Option<bool>,
}

// ============================================================================
// Request/Response Types
// ============================================================================

/// References request parameters
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceParams {
    #[serde(rename = "textDocument")]
    pub text_document: TextDocumentIdentifier,
    pub position: Position,
    pub context: ReferenceContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReferenceContext {
    #[serde(rename = "includeDeclaration")]
    pub include_declaration: bool,
}

/// Hover response
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hover {
    pub contents: HoverContents,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub range: Option<Range>,
}

/// Represents a MarkedString which can be a plain string or {language, value}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MarkedString {
    Plain(String),
    LanguageValue { language: String, value: String },
}

impl MarkedString {
    pub fn text(&self) -> &str {
        match self {
            MarkedString::Plain(s) => s,
            MarkedString::LanguageValue { value, .. } => value,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum HoverContents {
    Markup(MarkupContent),
    Array(Vec<MarkedString>),
    Scalar(MarkedString),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MarkupContent {
    pub kind: String,  // "plaintext" or "markdown"
    pub value: String,
}

/// Symbol information
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SymbolInformation {
    #[serde(default)]
    pub name: String,
    pub kind: SymbolKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<Location>,
    #[serde(skip_serializing_if = "Option::is_none", rename = "containerName")]
    pub container_name: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize, PartialEq)]
#[repr(u8)]
pub enum SymbolKind {
    File = 1,
    Module = 2,
    Namespace = 3,
    Package = 4,
    Class = 5,
    Method = 6,
    Property = 7,
    Field = 8,
    Constructor = 9,
    Enum = 10,
    Interface = 11,
    Function = 12,
    Variable = 13,
    Constant = 14,
    String = 15,
    Number = 16,
    Boolean = 17,
    Array = 18,
    Object = 19,
    Key = 20,
    Null = 21,
    EnumMember = 22,
    Struct = 23,
    Event = 24,
    Operator = 25,
    TypeParameter = 26,
}

// Custom deserializer to handle integer values from LSP servers
impl<'de> Deserialize<'de> for SymbolKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u8::deserialize(deserializer)?;
        match value {
            1 => Ok(SymbolKind::File),
            2 => Ok(SymbolKind::Module),
            3 => Ok(SymbolKind::Namespace),
            4 => Ok(SymbolKind::Package),
            5 => Ok(SymbolKind::Class),
            6 => Ok(SymbolKind::Method),
            7 => Ok(SymbolKind::Property),
            8 => Ok(SymbolKind::Field),
            9 => Ok(SymbolKind::Constructor),
            10 => Ok(SymbolKind::Enum),
            11 => Ok(SymbolKind::Interface),
            12 => Ok(SymbolKind::Function),
            13 => Ok(SymbolKind::Variable),
            14 => Ok(SymbolKind::Constant),
            15 => Ok(SymbolKind::String),
            16 => Ok(SymbolKind::Number),
            17 => Ok(SymbolKind::Boolean),
            18 => Ok(SymbolKind::Array),
            19 => Ok(SymbolKind::Object),
            20 => Ok(SymbolKind::Key),
            21 => Ok(SymbolKind::Null),
            22 => Ok(SymbolKind::EnumMember),
            23 => Ok(SymbolKind::Struct),
            24 => Ok(SymbolKind::Event),
            25 => Ok(SymbolKind::Operator),
            26 => Ok(SymbolKind::TypeParameter),
            _ => Err(serde::de::Error::custom(format!("Unknown SymbolKind: {}", value))),
        }
    }
}

/// Document symbol (hierarchical)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocumentSymbol {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    pub kind: SymbolKind,
    pub range: Range,
    #[serde(rename = "selectionRange")]
    pub selection_range: Range,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub children: Option<Vec<DocumentSymbol>>,
}

// ============================================================================
// JSON-RPC Types
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcRequest {
    pub jsonrpc: String,  // Always "2.0"
    pub id: u64,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcNotification {
    pub jsonrpc: String,
    pub method: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub params: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    /// Request ID — can be integer or string per JSON-RPC spec.
    /// We use Value to accept both forms from diverse LSP servers.
    pub id: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_server_capabilities_bool_form() {
        // gopls / rust-analyzer style: plain booleans
        let json = json!({
            "definitionProvider": true,
            "referencesProvider": true,
            "hoverProvider": false,
            "documentSymbolProvider": true,
            "workspaceSymbolProvider": true
        });

        let caps: ServerCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.definition_provider, Some(true));
        assert_eq!(caps.references_provider, Some(true));
        assert_eq!(caps.hover_provider, Some(false));
        assert_eq!(caps.document_symbol_provider, Some(true));
        assert_eq!(caps.workspace_symbol_provider, Some(true));
    }

    #[test]
    fn test_server_capabilities_object_form() {
        // pyright style: options objects with workDoneProgress
        let json = json!({
            "definitionProvider": {"workDoneProgress": true},
            "referencesProvider": {"workDoneProgress": true},
            "hoverProvider": {"workDoneProgress": true},
            "documentSymbolProvider": {"workDoneProgress": true},
            "workspaceSymbolProvider": {"workDoneProgress": true}
        });

        let caps: ServerCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.definition_provider, Some(true));
        assert_eq!(caps.references_provider, Some(true));
        assert_eq!(caps.hover_provider, Some(true));
        assert_eq!(caps.document_symbol_provider, Some(true));
        assert_eq!(caps.workspace_symbol_provider, Some(true));
    }

    #[test]
    fn test_server_capabilities_mixed_form() {
        // Some servers return a mix of bools and objects
        let json = json!({
            "definitionProvider": {"workDoneProgress": true},
            "referencesProvider": true,
            "hoverProvider": {"workDoneProgress": true},
            "workspaceSymbolProvider": false
        });

        let caps: ServerCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.definition_provider, Some(true));
        assert_eq!(caps.references_provider, Some(true));
        assert_eq!(caps.hover_provider, Some(true));
        assert_eq!(caps.document_symbol_provider, None); // absent
        assert_eq!(caps.workspace_symbol_provider, Some(false));
    }

    #[test]
    fn test_server_capabilities_absent_fields() {
        // Minimal response — no capability fields at all
        let json = json!({});

        let caps: ServerCapabilities = serde_json::from_value(json).unwrap();
        assert_eq!(caps.definition_provider, None);
        assert_eq!(caps.references_provider, None);
        assert_eq!(caps.hover_provider, None);
        assert_eq!(caps.document_symbol_provider, None);
        assert_eq!(caps.workspace_symbol_provider, None);
    }

    #[test]
    fn test_full_pyright_initialize_response() {
        // Real pyright 1.1.408 response (trimmed to capabilities we parse)
        let json = json!({
            "capabilities": {
                "textDocumentSync": 2,
                "definitionProvider": {"workDoneProgress": true},
                "referencesProvider": {"workDoneProgress": true},
                "documentSymbolProvider": {"workDoneProgress": true},
                "workspaceSymbolProvider": {"workDoneProgress": true},
                "hoverProvider": {"workDoneProgress": true},
                "callHierarchyProvider": true,
                "workspace": {"workspaceFolders": {"supported": true}}
            }
        });

        let result: InitializeResult = serde_json::from_value(json).unwrap();
        assert_eq!(result.capabilities.definition_provider, Some(true));
        assert_eq!(result.capabilities.references_provider, Some(true));
        assert_eq!(result.capabilities.hover_provider, Some(true));
        assert_eq!(result.capabilities.document_symbol_provider, Some(true));
        assert_eq!(result.capabilities.workspace_symbol_provider, Some(true));
    }
}
