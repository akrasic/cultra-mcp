//! MCP tool shim for `get_execution_waves` (CULTRA-1007).
//!
//! Thin forwarder — validates arguments and dispatches to the Go API's
//! `/api/v2/tasks/waves` endpoint. All graph-computation logic (Kahn's
//! algorithm, cycle detection, wave ordering) lives server-side in
//! `v2/internal/db/tasks.go::GetExecutionWaves`.
//!
//! Extracted from `tools.rs` per CULTRA-1017 (first of a broader
//! decomposition — see that ticket for rationale).

use anyhow::{anyhow, Result};
use serde_json::{Map, Value};

use crate::mcp::server::Server;
use crate::mcp::tools::validate_id;

/// Tool implementation: get_execution_waves
///
/// Arguments (exactly one scope required):
///   - `plan_id: string` — scope to a single plan's tasks
///   - `project_id: string` — scope to all tasks in a project
/// Optional:
///   - `include_statuses: [string]` — override default status filter
///   - `include_excluded: bool` — populate excluded.{done,cancelled,superseded}
pub(crate) fn get_execution_waves(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let plan_id = args.get("plan_id").and_then(|v| v.as_str());
    let project_id = args.get("project_id").and_then(|v| v.as_str());

    // Exactly one scope must be provided. The API enforces this too, but
    // rejecting at the shim layer gives a faster, clearer error.
    match (plan_id, project_id) {
        (None, None) => return Err(anyhow!(
            "exactly one of plan_id or project_id is required"
        )),
        (Some(_), Some(_)) => return Err(anyhow!(
            "pass only one of plan_id or project_id, not both"
        )),
        _ => {}
    }

    let mut query_params: Vec<(String, String)> = vec![];
    if let Some(pid) = plan_id {
        validate_id("plan_id", pid)?;
        query_params.push(("plan_id".to_string(), pid.to_string()));
    }
    if let Some(pid) = project_id {
        validate_id("project_id", pid)?;
        query_params.push(("project_id".to_string(), pid.to_string()));
    }

    // include_statuses: accept a JSON array of strings and join to comma-separated
    // for the query string. Matches the array idiom used elsewhere in tools.rs
    // (e.g. batch.operations); the Go handler splits on commas.
    if let Some(arr) = args.get("include_statuses").and_then(|v| v.as_array()) {
        let statuses: Vec<String> = arr
            .iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !statuses.is_empty() {
            query_params.push(("include_statuses".to_string(), statuses.join(",")));
        }
    }

    if let Some(flag) = args.get("include_excluded").and_then(|v| v.as_bool()) {
        if flag {
            query_params.push(("include_excluded".to_string(), "true".to_string()));
        }
    }

    server.api.get("/api/v2/tasks/waves", Some(query_params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::test_helpers::test_server;
    use serde_json::json;

    #[test]
    fn test_get_execution_waves_rejects_missing_scope() {
        let server = test_server();
        let err = get_execution_waves(&server, Map::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("plan_id") && msg.contains("project_id"),
            "expected error naming both scope params, got: {}", msg);
    }

    #[test]
    fn test_get_execution_waves_rejects_both_scopes() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("project_id".to_string(), json!("proj-x"));
        let err = get_execution_waves(&server, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("only one"),
            "expected 'only one' message when both scopes passed, got: {}", msg);
    }

    #[test]
    fn test_get_execution_waves_validates_plan_id() {
        // validate_id rejects ids with forbidden characters. A space will do.
        let server = test_server();
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("bad plan id"));
        let result = get_execution_waves(&server, args);
        assert!(result.is_err(), "expected validate_id to reject whitespace in plan_id");
    }

    #[test]
    fn test_get_execution_waves_accepts_valid_plan_id() {
        // No live API — we only care that validation passes. The subsequent
        // HTTP call will fail, but that failure is NOT a validation error.
        let server = test_server();
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-waves-demo"));
        let result = get_execution_waves(&server, args);
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(!msg.contains("plan_id") || !msg.contains("required"),
                "valid plan_id should pass validation, got: {}", msg);
        }
    }

    #[test]
    fn test_get_execution_waves_accepts_include_statuses_array() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("include_statuses".to_string(), json!(["todo", "in_progress"]));
        let result = get_execution_waves(&server, args);
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(!msg.contains("required") && !msg.contains("Invalid"),
                "valid include_statuses array should pass validation, got: {}", msg);
        }
    }

    #[test]
    fn test_get_execution_waves_accepts_include_excluded_bool() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("include_excluded".to_string(), json!(true));
        let result = get_execution_waves(&server, args);
        if let Err(e) = &result {
            let msg = e.to_string();
            assert!(!msg.contains("required") && !msg.contains("Invalid"),
                "valid include_excluded bool should pass validation, got: {}", msg);
        }
    }
}
