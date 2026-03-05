// LSP MCP Tools
//
// This module exposes LSP functionality as MCP tools callable from Claude Code.
// Tools use the shared LSPManager to reuse persistent language server connections
// instead of spawning a new process per call.

use super::manager::LSPManager;
use super::types::*;
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};

/// Helper to determine workspace root from arguments or current directory
fn get_workspace_root(args: &Map<String, Value>) -> Option<PathBuf> {
    args.get("workspace_root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// Helper to find the first source file with a matching extension in the workspace.
/// Searches up to 2 levels deep to avoid scanning huge trees.
fn find_source_file(root: &Path, extensions: &[&str]) -> Option<String> {
    // Check root directory first
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                    if extensions.contains(&ext) {
                        return path.to_str().map(|s| s.to_string());
                    }
                }
            }
        }
    }
    // Check one level of subdirectories
    if let Ok(entries) = std::fs::read_dir(root) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Ok(sub_entries) = std::fs::read_dir(&path) {
                    for sub_entry in sub_entries.flatten() {
                        let sub_path = sub_entry.path();
                        if sub_path.is_file() {
                            if let Some(ext) = sub_path.extension().and_then(|e| e.to_str()) {
                                if extensions.contains(&ext) {
                                    return sub_path.to_str().map(|s| s.to_string());
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    None
}

/// Get a client for a file, using the shared manager or creating an ad-hoc one
/// if the caller passes a different workspace_root.
fn get_client_for_file_and_open(
    lsp: &LSPManager,
    file_path: &str,
    args: &Map<String, Value>,
) -> Result<std::sync::Arc<std::sync::Mutex<super::client::LSPClient>>> {
    let custom_root = get_workspace_root(args);

    // If caller passes a different workspace_root, use cached ad-hoc client
    if let Some(ref root) = custom_root {
        if root != lsp.workspace_root() {
            let language = super::client::detect_language(file_path)
                .map_err(|e| anyhow!("Language detection failed: {}", e))?;
            let client_arc = lsp.get_or_create_adhoc_client(&language, root)
                .map_err(|e| anyhow!("Failed to create LSP client: {}", e))?;
            {
                let mut c = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                c.open_document(file_path)
                    .map_err(|e| anyhow!("Failed to open document: {}", e))?;
            }
            return Ok(client_arc);
        }
    }

    // Use the shared manager
    let client_arc = lsp.get_client_for_file(file_path)
        .map_err(|e| anyhow!("Failed to get LSP client: {}", e))?;

    {
        let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
        client.open_document(file_path)
            .map_err(|e| anyhow!("Failed to open document: {}", e))?;
    }

    Ok(client_arc)
}

// ============================================================================
// Consolidated LSP position query (references + definition + hover)
// ============================================================================

/// Consolidated LSP position query — handles references, definition, and hover
/// via a single entry point with an `action` parameter.
pub fn lsp_query(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: action"))?;

    // Validate action before doing any I/O
    match action {
        "references" | "definition" | "hover" => {}
        other => return Err(anyhow!("Invalid action '{}'. Must be 'references', 'definition', or 'hover'", other)),
    }

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;

    let line = args
        .get("line")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("Missing required parameter: line"))? as u32;

    let character = args
        .get("character")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| anyhow!("Missing required parameter: character"))? as u32;

    let client_arc = get_client_for_file_and_open(lsp, file_path, &args)?;
    let uri = super::client::file_uri(file_path);

    match action {
        "references" => {
            let params = ReferenceParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
                context: ReferenceContext { include_declaration: true },
            };
            let response = {
                let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                client.send_request("textDocument/references", Some(json!(params)))
                    .map_err(|e| anyhow!("LSP request failed: {}", e))?
            };
            if response.is_null() {
                return Ok(json!({"references": [], "count": 0}));
            }
            let locations: Vec<Location> = serde_json::from_value(response)
                .map_err(|e| anyhow!("Failed to parse references response: {}", e))?;
            Ok(json!({"references": locations, "count": locations.len()}))
        }
        "definition" => {
            let params = TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            };
            let response = {
                let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                client.send_request("textDocument/definition", Some(json!(params)))
                    .map_err(|e| anyhow!("LSP request failed: {}", e))?
            };
            if response.is_null() {
                return Ok(json!({"found": false, "message": "No definition found"}));
            }
            if let Ok(location) = serde_json::from_value::<Location>(response.clone()) {
                return Ok(json!({"found": true, "location": location}));
            }
            if let Ok(locations) = serde_json::from_value::<Vec<Location>>(response.clone()) {
                if let Some(first) = locations.first() {
                    return Ok(json!({"found": true, "location": first, "all_locations": locations}));
                }
            }
            Err(anyhow!("Unexpected definition response format"))
        }
        "hover" => {
            let params = TextDocumentPositionParams {
                text_document: TextDocumentIdentifier { uri },
                position: Position { line, character },
            };
            let response = {
                let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                client.send_request("textDocument/hover", Some(json!(params)))
                    .map_err(|e| anyhow!("LSP request failed: {}", e))?
            };
            if response.is_null() {
                return Ok(json!({"found": false, "message": "No hover information available"}));
            }
            let hover: Hover = serde_json::from_value(response)
                .map_err(|e| anyhow!("Failed to parse hover response: {}", e))?;
            let text = match hover.contents {
                HoverContents::Scalar(s) => s.text().to_string(),
                HoverContents::Array(arr) => arr.iter().map(|s| s.text()).collect::<Vec<_>>().join("\n\n"),
                HoverContents::Markup(m) => m.value,
            };
            Ok(json!({"found": true, "contents": text, "range": hover.range}))
        }
        _ => unreachable!("action already validated"),
    }
}

// ============================================================================
// Tool 4: lsp_workspace_symbols
// ============================================================================

/// Search for symbols across the entire workspace
pub fn lsp_workspace_symbols(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: language"))?;

    // For workspace symbols, we use get_or_create_client with the language directly.
    // If a custom workspace_root is provided and differs, create ad-hoc.
    let custom_root = get_workspace_root(&args);
    let client_arc = if let Some(ref root) = custom_root {
        if root != lsp.workspace_root() {
            lsp.get_or_create_adhoc_client(language, root)
                .map_err(|e| anyhow!("Failed to create LSP client: {}", e))?
        } else {
            lsp.get_or_create_client(language)
                .map_err(|e| anyhow!("Failed to get LSP client: {}", e))?
        }
    } else {
        lsp.get_or_create_client(language)
            .map_err(|e| anyhow!("Failed to get LSP client: {}", e))?
    };

    // Trigger workspace indexing if this is a fresh client (pyright needs didOpen first)
    {
        let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());

        // Check if the client has been used before by trying workspace/symbol first
        let test_params = json!({"query": ""});
        let test_response = client.send_request("workspace/symbol", Some(test_params))
            .map_err(|e| anyhow!("LSP request failed: {}", e))?;

        // If the test returned empty/null on a fresh client, trigger indexing
        let needs_indexing = test_response.is_null()
            || test_response.as_array().is_some_and(|a| a.is_empty());

        if needs_indexing {
            let extensions: &[&str] = match language {
                "python" => &["py"],
                "go" => &["go"],
                "rust" => &["rs"],
                "typescript" => &["ts", "tsx"],
                "javascript" => &["js", "jsx"],
                _ => &[],
            };

            let ws_root = custom_root.as_deref().unwrap_or(lsp.workspace_root());
            if let Some(file_path) = find_source_file(ws_root, extensions) {
                tracing::debug!("Opening file to trigger workspace indexing: {}", file_path);
                match client.open_document(&file_path) {
                    Ok(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(2500));
                    }
                    Err(e) => tracing::warn!("didOpen failed for workspace indexing: {}", e),
                }
            } else {
                tracing::warn!("No source file found for language '{}' in {:?}", language, ws_root);
            }
        }
    }

    let params = json!({"query": query});
    let response = {
        let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
        client.send_request("workspace/symbol", Some(params))
            .map_err(|e| anyhow!("LSP request failed: {}", e))?
    };

    if response.is_null() {
        return Ok(json!({
            "symbols": [],
            "count": 0
        }));
    }

    let symbols: Vec<SymbolInformation> = serde_json::from_value(response)
        .map_err(|e| anyhow!("Failed to parse workspace symbols response: {}", e))?;

    Ok(json!({
        "symbols": symbols,
        "count": symbols.len()
    }))
}

// ============================================================================
// Tool 5: lsp_document_symbols
// ============================================================================

/// List all symbols in a document
pub fn lsp_document_symbols(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;

    let client_arc = get_client_for_file_and_open(lsp, file_path, &args)?;

    let params = json!({
        "textDocument": {
            "uri": super::client::file_uri(file_path)
        }
    });

    let response = {
        let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
        client.send_request("textDocument/documentSymbol", Some(params))
            .map_err(|e| anyhow!("LSP request failed: {}", e))?
    };

    if response.is_null() {
        return Ok(json!({
            "symbols": [],
            "count": 0,
            "message": "No symbols found in document"
        }));
    }

    if let Ok(symbols) = serde_json::from_value::<Vec<DocumentSymbol>>(response.clone()) {
        return Ok(json!({
            "symbols": symbols,
            "count": symbols.len(),
            "format": "hierarchical"
        }));
    }

    if let Ok(symbols) = serde_json::from_value::<Vec<SymbolInformation>>(response.clone()) {
        return Ok(json!({
            "symbols": symbols,
            "count": symbols.len(),
            "format": "flat"
        }));
    }

    Err(anyhow!(
        "Unexpected documentSymbol response format. Response type: {}",
        if response.is_array() { "array" } else if response.is_object() { "object" } else { "other" }
    ))
}
