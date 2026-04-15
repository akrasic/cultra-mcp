// LSP MCP Tools
//
// This module exposes LSP functionality as MCP tools callable from Claude Code.
// Tools use the shared LSPManager to reuse persistent language server connections
// instead of spawning a new process per call.

use super::manager::LSPManager;
use super::types::*;
use crate::workspace::lsp_workspace_root_for_language;
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

/// Helper to determine workspace root from arguments or current directory
fn get_workspace_root(args: &Map<String, Value>) -> Option<PathBuf> {
    args.get("workspace_root")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// CULTRA-955: classify an LSP-backed tool's emptiness as warm/cold/unknown.
/// Mirrors the per-tool guards in find_dead_code/find_references but used
/// inline by lsp_document_symbols, lsp_workspace_symbols, and lsp_query —
/// the three pre-947 tools that originally shipped without the trust-debt
/// fix and were the subject of Tin-chan's QA sweep.
///
///   `checked`     — how many things did we ask the LSP about? 0 = unknown.
///   `useful_hits` — how many of those came back with information that's
///                   actionable for the caller? Empty/decl-only = 0.
///
/// `unknown` only applies to "we couldn't even ask the LSP anything"
/// (e.g. document with no symbols to query). For LSP shim tools, the
/// vast majority of calls land in `warm` or `cold`.
fn classify_lsp_emptiness(checked: usize, useful_hits: usize) -> &'static str {
    if checked == 0 {
        "unknown"
    } else if useful_hits > 0 {
        "warm"
    } else {
        "cold"
    }
}

/// CULTRA-971: paginate a symbol list with offset and optional max_results.
fn paginate_symbols(symbols: Vec<Value>, offset: usize, max_results: Option<usize>) -> Vec<Value> {
    let after_offset: Vec<Value> = symbols.into_iter().skip(offset).collect();
    match max_results {
        Some(max) => after_offset.into_iter().take(max).collect(),
        None => after_offset,
    }
}

/// CULTRA-955: build the standard cold-index metadata block for an
/// LSP-backed tool. Returns a Map with `lsp_index_status`, an optional
/// `warning` (only when status is "cold"), and a caveat. The caller
/// merges this into their response object.
///
/// `tool_kind`  — short name like "lsp_document_symbols" used in the
///                warning text so the user knows which tool produced it.
/// `query_desc` — human-readable description of what was queried, e.g.
///                "document 'foo.rs'" or "symbol query 'compose'".
fn build_cold_index_metadata(
    status: &str,
    tool_kind: &str,
    query_desc: &str,
) -> Map<String, Value> {
    let mut out = Map::new();
    out.insert("lsp_index_status".to_string(), json!(status));
    if status == "cold" {
        out.insert("warning".to_string(), json!(format!(
            "LSP index appears cold: {} returned no results for {}. \
             This is almost always an indexing gap (rust-analyzer / gopls / etc \
             still populating the cross-file index), not a real absence. \
             Either retry after the language server finishes indexing, \
             pass workspace_root explicitly to ensure the right project is \
             being indexed, or pass require_warm_index=true to fail fast \
             rather than receive best-effort empty results.",
            tool_kind, query_desc
        )));
    }
    out
}

/// CULTRA-955: resolve the LSP client for a given file, using the shared
/// workspace.rs walk-up to find the language workspace root. Falls back
/// to the manager's default workspace_root only if the walk-up returns
/// nothing (legacy behavior). Result is the same shape as
/// get_client_for_file_and_open returns.
///
/// This is the core fix for the "MCP cwd defaults are wrong for nested
/// crates" family of bugs that CULTRA-952 first hit in the warmup path.
fn resolve_lsp_client_for_file(
    lsp: &LSPManager,
    file_path: &str,
    args: &Map<String, Value>,
) -> Result<std::sync::Arc<std::sync::Mutex<super::client::LSPClient>>> {
    // Caller-provided workspace_root always wins.
    if let Some(root) = get_workspace_root(args) {
        let language = super::client::detect_language(file_path)
            .map_err(|e| anyhow!("Language detection failed: {}", e))?;
        return lsp.get_or_create_adhoc_client(&language, &root)
            .map_err(|e| anyhow!("Failed to create LSP client at {}: {}", root.display(), e));
    }

    // Walk up from file_path looking for the language's workspace anchor.
    let language = super::client::detect_language(file_path)
        .map_err(|e| anyhow!("Language detection failed: {}", e))?;
    let abs_path = Path::new(file_path);
    if let Some(workspace) = lsp_workspace_root_for_language(&language, abs_path, lsp.workspace_root()) {
        return lsp.get_or_create_adhoc_client(&language, &workspace.root)
            .map_err(|e| anyhow!("Failed to create LSP client at {}: {}", workspace.root.display(), e));
    }

    // Fallback: legacy behavior for layouts where walk-up finds nothing
    // (no manifest in any ancestor up to the sandbox root).
    lsp.get_client_for_file(file_path)
        .map_err(|e| anyhow!("Failed to get LSP client: {}", e))
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

/// Get a client for a file and open the document on it.
///
/// CULTRA-955: workspace_root resolution goes through resolve_lsp_client_for_file,
/// which prefers an explicit `workspace_root` arg, then walks up from
/// file_path looking for the language's manifest, and only falls back to
/// the manager's default cwd as a last resort. The pre-955 behavior was
/// "default to manager.workspace_root() (== MCP cwd) and silently return
/// empty if rust-analyzer was pointed at a non-workspace directory."
fn get_client_for_file_and_open(
    lsp: &LSPManager,
    file_path: &str,
    args: &Map<String, Value>,
) -> Result<std::sync::Arc<std::sync::Mutex<super::client::LSPClient>>> {
    let client_arc = resolve_lsp_client_for_file(lsp, file_path, args)?;
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
///
/// CULTRA-955: cold-index guard applied to all three actions.
/// `references` uses the find_references-style classification (any
/// non-declaration reference = warm). `definition` and `hover` use the
/// simpler "got something / didn't" classification — both have a known
/// trap where "symbol not found" is indistinguishable from "index cold,"
/// so the warning text covers both cases. workspace_root resolution goes
/// through get_client_for_file_and_open's resolve_lsp_client_for_file path.
pub fn lsp_query(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: action"))?;

    // Validate action before doing any I/O
    match action {
        "references" | "definition" | "hover" | "implementation" => {}
        other => return Err(anyhow!("Invalid action '{}'. Must be 'references', 'definition', 'hover', or 'implementation'", other)),
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

    let require_warm_index = args
        .get("require_warm_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-963: optional active warmup, same semantics as find_references
    // and find_dead_code. Runs the per-language warmup command (cargo check /
    // go build / tsc --noEmit) before querying LSP so the index is populated
    // for positional queries (hover, definition) that need full semantic
    // analysis, not just declaration-level info.
    let do_warmup = args
        .get("warmup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let warmup_report: Option<super::manager::WarmupReport> = if do_warmup {
        let language = super::client::detect_language(file_path)
            .map_err(|e| anyhow!("Language detection failed: {}", e))?;
        Some(lsp.ensure_warm(&language, Path::new(file_path)))
    } else {
        None
    };

    let client_arc = get_client_for_file_and_open(lsp, file_path, &args)?;
    let uri = super::client::file_uri(file_path);

    // Inner helper: execute the LSP query once and return (body, useful_hits, query_desc).
    // Extracted so the retry loop below can re-execute without duplicating the match.
    let execute_query = |client_arc: &std::sync::Arc<std::sync::Mutex<super::client::LSPClient>>,
                         uri: &str, action: &str, file_path: &str, line: u32, character: u32|
        -> Result<(Value, usize, String)>
    {
        match action {
            "references" => {
                let params = ReferenceParams {
                    text_document: TextDocumentIdentifier { uri: uri.to_string() },
                    position: Position { line, character },
                    context: ReferenceContext { include_declaration: true },
                };
                let response = {
                    let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                    client.send_request("textDocument/references", Some(json!(params)))
                        .map_err(|e| anyhow!("LSP request failed: {}", e))?
                };
                let locations: Vec<Location> = if response.is_null() {
                    Vec::new()
                } else {
                    serde_json::from_value(response)
                        .map_err(|e| anyhow!("Failed to parse references response: {}", e))?
                };
                let non_decl_count = locations.iter().filter(|loc| {
                    let same_file = loc.uri.ends_with(file_path);
                    let same_line = loc.range.start.line == line;
                    !(same_file && same_line)
                }).count();
                let count = locations.len();
                Ok((
                    json!({"references": locations, "count": count}),
                    non_decl_count,
                    format!("references at {}:{}:{}", file_path, line, character),
                ))
            }
            "definition" => {
                let params = TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.to_string() },
                    position: Position { line, character },
                };
                let response = {
                    let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                    client.send_request("textDocument/definition", Some(json!(params)))
                        .map_err(|e| anyhow!("LSP request failed: {}", e))?
                };
                if response.is_null() {
                    Ok((
                        json!({"found": false, "message": "No definition found"}),
                        0,
                        format!("definition at {}:{}:{}", file_path, line, character),
                    ))
                } else if let Ok(location) = serde_json::from_value::<Location>(response.clone()) {
                    Ok((
                        json!({"found": true, "location": location}),
                        1,
                        format!("definition at {}:{}:{}", file_path, line, character),
                    ))
                } else if let Ok(locations) = serde_json::from_value::<Vec<Location>>(response.clone()) {
                    if let Some(first) = locations.first() {
                        let n = locations.len();
                        Ok((
                            json!({"found": true, "location": first, "all_locations": locations}),
                            n,
                            format!("definition at {}:{}:{}", file_path, line, character),
                        ))
                    } else {
                        Ok((
                            json!({"found": false, "message": "No definition found"}),
                            0,
                            format!("definition at {}:{}:{}", file_path, line, character),
                        ))
                    }
                } else {
                    Err(anyhow!("Unexpected definition response format"))
                }
            }
            // CULTRA-966: textDocument/implementation — find trait implementations
            // (Rust), interface implementations (Go/TS). Same request/response shape
            // as definition (TextDocumentPositionParams → Location | Location[]).
            "implementation" => {
                let params = TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.to_string() },
                    position: Position { line, character },
                };
                let response = {
                    let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                    client.send_request("textDocument/implementation", Some(json!(params)))
                        .map_err(|e| anyhow!("LSP request failed: {}", e))?
                };
                if response.is_null() {
                    Ok((
                        json!({"found": false, "implementations": [], "count": 0, "message": "No implementations found"}),
                        0,
                        format!("implementation at {}:{}:{}", file_path, line, character),
                    ))
                } else if let Ok(location) = serde_json::from_value::<Location>(response.clone()) {
                    Ok((
                        json!({"found": true, "implementations": [location], "count": 1}),
                        1,
                        format!("implementation at {}:{}:{}", file_path, line, character),
                    ))
                } else if let Ok(locations) = serde_json::from_value::<Vec<Location>>(response.clone()) {
                    let count = locations.len();
                    Ok((
                        json!({"found": count > 0, "implementations": locations, "count": count}),
                        count,
                        format!("implementation at {}:{}:{}", file_path, line, character),
                    ))
                } else {
                    Err(anyhow!("Unexpected implementation response format"))
                }
            }
            "hover" => {
                let params = TextDocumentPositionParams {
                    text_document: TextDocumentIdentifier { uri: uri.to_string() },
                    position: Position { line, character },
                };
                let response = {
                    let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
                    client.send_request("textDocument/hover", Some(json!(params)))
                        .map_err(|e| anyhow!("LSP request failed: {}", e))?
                };
                if response.is_null() {
                    Ok((
                        json!({"found": false, "message": "No hover information available"}),
                        0,
                        format!("hover at {}:{}:{}", file_path, line, character),
                    ))
                } else {
                    let hover: Hover = serde_json::from_value(response)
                        .map_err(|e| anyhow!("Failed to parse hover response: {}", e))?;
                    let text = match hover.contents {
                        HoverContents::Scalar(s) => s.text().to_string(),
                        HoverContents::Array(arr) => arr.iter().map(|s| s.text()).collect::<Vec<_>>().join("\n\n"),
                        HoverContents::Markup(m) => m.value,
                    };
                    let useful = if text.trim().is_empty() { 0 } else { 1 };
                    Ok((
                        json!({"found": useful > 0, "contents": text, "range": hover.range}),
                        useful,
                        format!("hover at {}:{}:{}", file_path, line, character),
                    ))
                }
            }
            _ => unreachable!("action already validated"),
        }
    };

    // Execute the query, retrying if warmup was requested but the index is cold.
    // CULTRA-963: when warmup=true, the caller expects a warm answer. Rather than
    // returning a cold result immediately (the warmup race — cargo check / go build
    // finishes before the language server's internal index is ready), poll until the
    // index catches up. Timeout after 90s to avoid hanging indefinitely.
    let (mut body, useful_hits, query_desc) = execute_query(&client_arc, &uri, action, file_path, line, character)?;
    let mut status = classify_lsp_emptiness(1, useful_hits);

    if do_warmup && status == "cold" {
        let retry_timeout: u64 = std::env::var("CULTRA_LSP_RETRY_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(90);
        let deadline = Instant::now() + Duration::from_secs(retry_timeout);
        let retry_interval = Duration::from_secs(3);

        // Readiness probe strategy: documentSymbol returns early in the
        // indexing lifecycle (structure-level), but hover/definition need
        // deeper semantic analysis. So we probe by hovering on a known
        // symbol name from the documentSymbol results. If that hover succeeds,
        // the semantic index is warm — an empty result for the original
        // position means the position genuinely has nothing hoverable.
        //
        // Key subtlety: documentSymbol range.start points to the declaration
        // keyword (e.g. `pub` in `pub struct Foo`), NOT the symbol name.
        // Hovering on `pub` returns nothing even on a warm index. We must
        // find the symbol name's actual column by searching the source line.
        let source_lines: Vec<String> = std::fs::read_to_string(file_path)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default();

        let probe_position: Option<(u32, u32)> = {
            let sym_params = json!({
                "textDocument": { "uri": super::client::file_uri(file_path) }
            });
            let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
            client.send_request("textDocument/documentSymbol", Some(sym_params))
                .ok()
                .and_then(|resp| resp.as_array().cloned())
                .and_then(|symbols| {
                    symbols.iter().find_map(|sym| {
                        let kind = sym.get("kind").and_then(|k| k.as_u64()).unwrap_or(0);
                        // LSP SymbolKind: 5=Class, 6=Method, 12=Function, 23=Struct
                        if !matches!(kind, 5 | 6 | 12 | 23) {
                            return None;
                        }
                        let name = sym.get("name").and_then(|n| n.as_str())?;
                        let range = sym.get("location")
                            .and_then(|l| l.get("range"))
                            .or_else(|| sym.get("range"));
                        let start_line = range
                            .and_then(|r| r.get("start"))
                            .and_then(|s| s.get("line"))
                            .and_then(|v| v.as_u64())? as usize;
                        // Find the name's column in the source line.
                        let col = source_lines.get(start_line)
                            .and_then(|l| l.find(name))? as u32;
                        Some((start_line as u32, col))
                    })
                })
        };

        while status == "cold" && Instant::now() < deadline {
            std::thread::sleep(retry_interval);

            // If we have a probe position, test if semantic indexing is ready
            // by hovering on a known symbol. If the probe succeeds but the
            // original query is empty, the position genuinely has nothing.
            if let Some((probe_line, probe_char)) = probe_position {
                let probe_warm = execute_query(
                    &client_arc, &uri, "hover", file_path, probe_line, probe_char
                ).map(|(_, hits, _)| hits > 0).unwrap_or(false);

                if probe_warm {
                    // Semantic index is ready. Re-execute the original query
                    // one final time — if still empty, it's genuinely empty.
                    if let Ok((new_body, new_hits, _)) = execute_query(
                        &client_arc, &uri, action, file_path, line, character
                    ) {
                        body = new_body;
                        status = classify_lsp_emptiness(1, new_hits);
                    }
                    break;
                }
            }

            // No probe or probe not ready yet — retry the actual query directly.
            match execute_query(&client_arc, &uri, action, file_path, line, character) {
                Ok((new_body, new_hits, _new_desc)) => {
                    body = new_body;
                    status = classify_lsp_emptiness(1, new_hits);
                }
                Err(_) => break,
            }
        }
    }
    if status == "cold" && require_warm_index {
        return Err(anyhow!(
            "LSP index is cold for {}: returned no useful results. \
             This usually means the language server has not yet indexed the workspace. \
             Either retry after indexing finishes or pass require_warm_index=false.",
            query_desc
        ));
    }
    let metadata = build_cold_index_metadata(status, &format!("lsp({})", action), &query_desc);
    if let Value::Object(ref mut obj) = body {
        for (k, v) in metadata { obj.insert(k, v); }
        // CULTRA-963: surface warmup report so callers can see whether warmup
        // ran, whether it was cached, and how long it took.
        if let Some(ref report) = warmup_report {
            obj.insert("warmup_report".to_string(), json!({
                "status": report.status,
                "cached": report.cached,
                "elapsed_ms": report.elapsed_ms,
                "language": report.language,
                "command": report.command,
                "message": report.message,
            }));
        }
    }
    Ok(body)
}

// ============================================================================
// Tool 4: lsp_workspace_symbols
// ============================================================================

/// Search for symbols across the entire workspace.
///
/// CULTRA-955: workspace_root resolution + cold-index guard. The pre-fix
/// version defaulted to `lsp.workspace_root()` (== MCP cwd) when neither
/// `workspace_root` nor a hint was provided, which Tin-chan's QA showed
/// silently failed on first call of any session in a nested-crate layout.
/// The fix adds an optional `file_path` hint: if given, we walk up from it
/// to the language workspace root the same way lsp_document_symbols does.
/// Resolution priority: explicit `workspace_root` > walk-up from `file_path` >
/// manager default (with a cold-index warning when fallback hits empty).
pub fn lsp_workspace_symbols(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    let language = args
        .get("language")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: language"))?;

    let require_warm_index = args
        .get("require_warm_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-963: optional active warmup. For workspace_symbols the warmup
    // target is the file_path hint (if provided) or a synthetic path inside
    // the resolved root so ensure_warm can find the manifest.
    let do_warmup = args
        .get("warmup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-955 resolution order: explicit workspace_root > file_path
    // hint walk-up > manager default.
    let custom_root = get_workspace_root(&args);
    let resolved_root: PathBuf = if let Some(ref root) = custom_root {
        root.clone()
    } else if let Some(hint) = args.get("file_path").and_then(|v| v.as_str()) {
        let abs_hint = Path::new(hint);
        match lsp_workspace_root_for_language(language, abs_hint, lsp.workspace_root()) {
            Some(ws) => ws.root,
            None => lsp.workspace_root().to_path_buf(),
        }
    } else {
        lsp.workspace_root().to_path_buf()
    };

    let warmup_report: Option<super::manager::WarmupReport> = if do_warmup {
        // Use file_path hint if provided, otherwise synthesize a path inside
        // the resolved root so ensure_warm can walk up to the manifest.
        let warmup_path = args.get("file_path")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| resolved_root.join("dummy.rs"));
        Some(lsp.ensure_warm(language, &warmup_path))
    } else {
        None
    };

    let client_arc = if resolved_root == lsp.workspace_root() {
        lsp.get_or_create_client(language)
            .map_err(|e| anyhow!("Failed to get LSP client: {}", e))?
    } else {
        lsp.get_or_create_adhoc_client(language, &resolved_root)
            .map_err(|e| anyhow!("Failed to create LSP client at {}: {}", resolved_root.display(), e))?
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
                "svelte" => &["svelte"],
                _ => &[],
            };

            // CULTRA-955: indexing trigger must use the resolved root, not
            // the manager default — otherwise rust-analyzer opens a file
            // outside the project it was just spawned for.
            if let Some(file_path) = find_source_file(&resolved_root, extensions) {
                tracing::debug!("Opening file to trigger workspace indexing: {}", file_path);
                match client.open_document(&file_path) {
                    Ok(_) => {
                        std::thread::sleep(std::time::Duration::from_millis(2500));
                    }
                    Err(e) => tracing::warn!("didOpen failed for workspace indexing: {}", e),
                }
            } else {
                tracing::warn!("No source file found for language '{}' in {:?}", language, resolved_root);
            }
        }
    }

    let params = json!({"query": query});
    let response = {
        let mut client = client_arc.lock().unwrap_or_else(|e| e.into_inner());
        client.send_request("workspace/symbol", Some(params))
            .map_err(|e| anyhow!("LSP request failed: {}", e))?
    };

    let symbols: Vec<SymbolInformation> = if response.is_null() {
        Vec::new()
    } else {
        serde_json::from_value(response)
            .map_err(|e| anyhow!("Failed to parse workspace symbols response: {}", e))?
    };

    // CULTRA-965: deduplicate. rust-analyzer can return the same symbol
    // multiple times when the workspace root contains nested crates or
    // when a symbol is re-exported. Dedup by (name, kind, uri, start_line).
    let symbols = {
        let mut seen = std::collections::HashSet::new();
        symbols.into_iter().filter(|s| {
            let loc_key = s.location.as_ref().map(|l| {
                format!("{}:{}", l.uri, l.range.start.line)
            }).unwrap_or_default();
            let key = format!("{}:{}:{}", s.name, s.kind as u32, loc_key);
            seen.insert(key)
        }).collect::<Vec<_>>()
    };

    let count = symbols.len();
    let mut body = json!({
        "symbols": symbols,
        "count": count,
    });

    // CULTRA-955: cold-index guard. checked=1 (one workspace query). The
    // empty case here is genuinely ambiguous between "query matched
    // nothing" and "index not ready" — but a confidently-empty result is
    // more dangerous than a noisy warning, so we err on flagging cold.
    let status = classify_lsp_emptiness(1, count);
    if status == "cold" && require_warm_index {
        return Err(anyhow!(
            "LSP index is cold for workspace query '{}': returned 0 symbols. \
             This usually means the language server has not yet indexed the workspace, \
             OR the query genuinely matches nothing. Either retry after indexing or \
             pass require_warm_index=false to accept best-effort results.",
            query
        ));
    }
    let metadata = build_cold_index_metadata(
        status,
        "lsp_workspace_symbols",
        &format!("query '{}'", query),
    );
    if let Value::Object(ref mut obj) = body {
        for (k, v) in metadata { obj.insert(k, v); }
        if let Some(ref report) = warmup_report {
            obj.insert("warmup_report".to_string(), json!({
                "status": report.status,
                "cached": report.cached,
                "elapsed_ms": report.elapsed_ms,
                "language": report.language,
                "command": report.command,
                "message": report.message,
            }));
        }
    }
    Ok(body)
}

// ============================================================================
// Tool 5: lsp_document_symbols
// ============================================================================

/// List all symbols in a document.
///
/// CULTRA-955: workspace_root resolution + cold-index guard. The pre-fix
/// version returned `{count: 0, message: "No symbols found in document"}`
/// when rust-analyzer was pointed at the wrong workspace, which Tin-chan's
/// QA sweep flagged as a confident-empty-result trap (the file had 186
/// symbols when called with the right workspace_root). The fix:
/// (1) walks up from file_path to resolve the LSP workspace root via
///     get_client_for_file_and_open's resolve_lsp_client_for_file path,
/// (2) wraps an empty response with the cold-index guard so the user
///     sees `lsp_index_status: "cold"` + a warning instead of a confident
///     empty count.
pub fn lsp_document_symbols(args: Map<String, Value>, lsp: &LSPManager) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;

    let require_warm_index = args
        .get("require_warm_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-971: pagination to avoid overflowing the MCP response on large files.
    let max_results = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);
    let offset = args
        .get("offset")
        .and_then(|v| v.as_u64())
        .unwrap_or(0) as usize;

    // CULTRA-963: optional active warmup, same as lsp_query.
    let do_warmup = args
        .get("warmup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let warmup_report: Option<super::manager::WarmupReport> = if do_warmup {
        let language = super::client::detect_language(file_path)
            .map_err(|e| anyhow!("Language detection failed: {}", e))?;
        Some(lsp.ensure_warm(&language, Path::new(file_path)))
    } else {
        None
    };

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

    // Build the base response object based on what the LSP returned, then
    // attach cold-index metadata at the end.
    let (mut body, useful_hits) = if response.is_null() {
        (
            json!({
                "symbols": [],
                "count": 0,
                "message": "No symbols found in document",
            }),
            0,
        )
    } else if let Ok(symbols) = serde_json::from_value::<Vec<DocumentSymbol>>(response.clone()) {
        let total = symbols.len();
        // CULTRA-971: convert hierarchical to flat JSON values for pagination.
        // Hierarchical DocumentSymbol trees can be deeply nested; flattening
        // makes offset/max_results semantics simple and predictable.
        let flat_values: Vec<Value> = serde_json::to_value(&symbols)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let page = paginate_symbols(flat_values, offset, max_results);
        let page_count = page.len();
        let mut resp = json!({
            "symbols": page,
            "count": page_count,
            "total": total,
            "format": "hierarchical",
        });
        if offset > 0 || max_results.is_some() {
            if let Value::Object(ref mut obj) = resp {
                obj.insert("offset".to_string(), json!(offset));
                if let Some(max) = max_results {
                    obj.insert("max_results".to_string(), json!(max));
                }
            }
        }
        (resp, total)
    } else if let Ok(symbols) = serde_json::from_value::<Vec<SymbolInformation>>(response.clone()) {
        let total = symbols.len();
        let flat_values: Vec<Value> = serde_json::to_value(&symbols)
            .ok()
            .and_then(|v| v.as_array().cloned())
            .unwrap_or_default();
        let page = paginate_symbols(flat_values, offset, max_results);
        let page_count = page.len();
        let mut resp = json!({
            "symbols": page,
            "count": page_count,
            "total": total,
            "format": "flat",
        });
        if offset > 0 || max_results.is_some() {
            if let Value::Object(ref mut obj) = resp {
                obj.insert("offset".to_string(), json!(offset));
                if let Some(max) = max_results {
                    obj.insert("max_results".to_string(), json!(max));
                }
            }
        }
        (resp, total)
    } else {
        return Err(anyhow!(
            "Unexpected documentSymbol response format. Response type: {}",
            if response.is_array() { "array" } else if response.is_object() { "object" } else { "other" }
        ));
    };

    // CULTRA-955: cold-index guard. checked=1 (we asked about one document).
    let status = classify_lsp_emptiness(1, useful_hits);
    if status == "cold" && require_warm_index {
        return Err(anyhow!(
            "LSP index is cold for document '{}': document_symbol returned 0 symbols. \
             This usually means the language server has not yet indexed the workspace. \
             Either retry after indexing finishes or pass require_warm_index=false to \
             accept best-effort results.",
            file_path
        ));
    }
    let metadata = build_cold_index_metadata(
        status,
        "lsp_document_symbols",
        &format!("document '{}'", file_path),
    );
    if let Value::Object(ref mut obj) = body {
        for (k, v) in metadata { obj.insert(k, v); }
        if let Some(ref report) = warmup_report {
            obj.insert("warmup_report".to_string(), json!({
                "status": report.status,
                "cached": report.cached,
                "elapsed_ms": report.elapsed_ms,
                "language": report.language,
                "command": report.command,
                "message": report.message,
            }));
        }
    }
    Ok(body)
}

// ============================================================================
// Tests (CULTRA-955)
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_lsp_emptiness_warm() {
        assert_eq!(classify_lsp_emptiness(1, 1), "warm");
        assert_eq!(classify_lsp_emptiness(5, 3), "warm");
        assert_eq!(classify_lsp_emptiness(100, 1), "warm");
    }

    #[test]
    fn test_classify_lsp_emptiness_cold() {
        assert_eq!(classify_lsp_emptiness(1, 0), "cold");
        assert_eq!(classify_lsp_emptiness(5, 0), "cold");
    }

    #[test]
    fn test_classify_lsp_emptiness_unknown_when_nothing_checked() {
        assert_eq!(classify_lsp_emptiness(0, 0), "unknown");
    }

    #[test]
    fn test_build_cold_index_metadata_warm_no_warning() {
        let m = build_cold_index_metadata("warm", "lsp_document_symbols", "document 'foo.rs'");
        assert_eq!(m.get("lsp_index_status").unwrap(), "warm");
        assert!(m.get("warning").is_none(), "warm status must not emit a warning");
    }

    #[test]
    fn test_build_cold_index_metadata_cold_includes_warning() {
        let m = build_cold_index_metadata("cold", "lsp_workspace_symbols", "query 'compose'");
        assert_eq!(m.get("lsp_index_status").unwrap(), "cold");
        let warning = m.get("warning").and_then(|v| v.as_str()).unwrap();
        assert!(warning.contains("lsp_workspace_symbols"), "warning should name the tool: {}", warning);
        assert!(warning.contains("compose"), "warning should name the query: {}", warning);
        assert!(warning.contains("require_warm_index"), "warning should mention the strict-mode opt-in: {}", warning);
        assert!(warning.contains("workspace_root"), "warning should suggest workspace_root override: {}", warning);
    }

    #[test]
    fn test_build_cold_index_metadata_unknown_no_warning() {
        let m = build_cold_index_metadata("unknown", "lsp_query", "references at foo.rs:10:5");
        assert_eq!(m.get("lsp_index_status").unwrap(), "unknown");
        assert!(m.get("warning").is_none());
    }

    #[test]
    fn test_resolve_lsp_client_walks_up_from_file_path() {
        // CULTRA-955 reproduction: sandbox at outer dir, crate at sandbox/vux,
        // file at sandbox/vux/src/main.rs. The pre-fix code would point
        // rust-analyzer at the sandbox; the fix walks up to find Cargo.toml.
        // We don't actually spawn rust-analyzer in tests — we just verify
        // the resolver picks the right workspace root by intercepting via
        // the workspace::lsp_workspace_root_for_language function.
        use crate::workspace::lsp_workspace_root_for_language;
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().join("vux");
        let src_dir = crate_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"vux\"\n").unwrap();
        let src = src_dir.join("main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();

        let resolved = lsp_workspace_root_for_language("rust", &src, dir.path()).unwrap();
        assert_eq!(resolved.root, crate_dir.canonicalize().unwrap(),
            "should resolve to the crate dir, not the sandbox root");
    }
}
