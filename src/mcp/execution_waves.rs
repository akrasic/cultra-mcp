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

/// Validate arguments and build the query-param list for the `/api/v2/tasks/waves`
/// call. Split from `get_execution_waves` so the argument-handling path is unit-
/// testable without the HTTP client — otherwise validation-pass is indistinguish-
/// able from network-fail in tests (CULTRA-1022).
fn build_waves_query_params(args: &Map<String, Value>) -> Result<Vec<(String, String)>> {
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

    // CULTRA-1050: optional ASCII rendering. The Go handler treats unknown
    // format values as "no rendering" — we still validate here so the agent
    // gets a clear error rather than silently-ignored params.
    if let Some(fmt) = args.get("format").and_then(|v| v.as_str()) {
        if fmt != "ascii" {
            return Err(anyhow!(
                "invalid format '{}'. Only 'ascii' is supported (omit for the default structured response)",
                fmt
            ));
        }
        query_params.push(("format".to_string(), fmt.to_string()));
    }

    // The remaining knobs are presentation-only; bad values silently fall
    // back to defaults at the renderer. We mirror the renderer's leniency at
    // the shim layer EXCEPT for clearly-wrong types — those would be a sign
    // the caller misunderstands the schema.
    if let Some(w) = args.get("width") {
        let n = w
            .as_u64()
            .ok_or_else(|| anyhow!("width must be a positive integer"))?;
        if n == 0 {
            return Err(anyhow!("width must be > 0"));
        }
        query_params.push(("width".to_string(), n.to_string()));
    }

    if let Some(s) = args.get("style").and_then(|v| v.as_str()) {
        if s != "unicode" && s != "ascii" {
            return Err(anyhow!(
                "invalid style '{}'. Must be 'unicode' or 'ascii'",
                s
            ));
        }
        query_params.push(("style".to_string(), s.to_string()));
    }

    if let Some(flag) = args.get("with_titles").and_then(|v| v.as_bool()) {
        if flag {
            query_params.push(("with_titles".to_string(), "true".to_string()));
        }
    }

    // CULTRA-1059: with_handles=false drops T-handles. Default (absent or
    // true) preserves historical output, so we only emit the param when
    // explicitly false.
    if let Some(flag) = args.get("with_handles").and_then(|v| v.as_bool()) {
        if !flag {
            query_params.push(("with_handles".to_string(), "false".to_string()));
        }
    }

    // CULTRA-1069: compact_parallel=true collapses N independent components
    // into a single combined wave stanza. Default off preserves historical
    // partitioned form. Only emit when explicitly true.
    if let Some(flag) = args.get("compact_parallel").and_then(|v| v.as_bool()) {
        if flag {
            query_params.push(("compact_parallel".to_string(), "true".to_string()));
        }
    }

    Ok(query_params)
}

/// Tool implementation: get_execution_waves
///
/// Arguments (exactly one scope required):
///   - `plan_id: string` — scope to a single plan's tasks
///   - `project_id: string` — scope to all tasks in a project
/// Optional:
///   - `include_statuses: [string]` — override default status filter
///   - `include_excluded: bool` — populate excluded.{done,cancelled,superseded}
pub(crate) fn get_execution_waves(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let query_params = build_waves_query_params(&args)?;
    server.api.get("/api/v2/tasks/waves", Some(query_params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Tests target build_waves_query_params — a pure function with no HTTP
    // dependency. This lets us assert exactly what the query layer would send,
    // distinguishing validation-pass from network-fail. A silently-ignored
    // parameter fails these tests because the expected (key, value) would
    // be absent from the returned vec.

    #[test]
    fn test_build_waves_query_params_rejects_missing_scope() {
        let err = build_waves_query_params(&Map::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("plan_id") && msg.contains("project_id"),
            "expected error naming both scope params, got: {}", msg);
    }

    #[test]
    fn test_build_waves_query_params_rejects_both_scopes() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("project_id".to_string(), json!("proj-x"));
        let err = build_waves_query_params(&args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("only one"),
            "expected 'only one' message when both scopes passed, got: {}", msg);
    }

    #[test]
    fn test_build_waves_query_params_validates_plan_id() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("bad plan id"));
        let err = build_waves_query_params(&args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("plan_id"),
            "expected error naming plan_id, got: {}", msg);
    }

    #[test]
    fn test_build_waves_query_params_emits_plan_id() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-waves-demo"));
        let params = build_waves_query_params(&args).expect("valid plan_id should pass");
        assert_eq!(params, vec![("plan_id".to_string(), "plan-waves-demo".to_string())],
            "plan_id should appear in query params exactly once, with its original value");
    }

    #[test]
    fn test_build_waves_query_params_emits_project_id() {
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        let params = build_waves_query_params(&args).expect("valid project_id should pass");
        assert_eq!(params, vec![("project_id".to_string(), "proj-x".to_string())]);
    }

    #[test]
    fn test_build_waves_query_params_joins_include_statuses_array() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("include_statuses".to_string(), json!(["todo", "in_progress"]));
        let params = build_waves_query_params(&args).expect("valid args should pass");
        let include_statuses = params.iter()
            .find(|(k, _)| k == "include_statuses")
            .expect("include_statuses should be present in query params");
        assert_eq!(include_statuses.1, "todo,in_progress",
            "include_statuses should be comma-joined in the order given");
    }

    #[test]
    fn test_build_waves_query_params_drops_empty_include_statuses() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("include_statuses".to_string(), json!([]));
        let params = build_waves_query_params(&args).expect("empty array is valid");
        assert!(params.iter().all(|(k, _)| k != "include_statuses"),
            "empty include_statuses should be omitted, not sent as blank");
    }

    #[test]
    fn test_build_waves_query_params_emits_include_excluded_when_true() {
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("include_excluded".to_string(), json!(true));
        let params = build_waves_query_params(&args).expect("valid args should pass");
        let include_excluded = params.iter()
            .find(|(k, _)| k == "include_excluded")
            .expect("include_excluded=true should be present in query params");
        assert_eq!(include_excluded.1, "true");
    }

    #[test]
    fn test_build_waves_query_params_drops_include_excluded_when_false() {
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("include_excluded".to_string(), json!(false));
        let params = build_waves_query_params(&args).expect("valid args should pass");
        assert!(params.iter().all(|(k, _)| k != "include_excluded"),
            "include_excluded=false should be omitted (default), not sent explicitly");
    }

    // CULTRA-1050: ASCII rendering passthrough.

    #[test]
    fn test_build_waves_query_params_emits_format_ascii() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("format".to_string(), json!("ascii"));
        let params = build_waves_query_params(&args).expect("format=ascii is valid");
        let format = params.iter()
            .find(|(k, _)| k == "format")
            .expect("format=ascii should pass through");
        assert_eq!(format.1, "ascii");
    }

    #[test]
    fn test_build_waves_query_params_rejects_unknown_format() {
        // Silently ignoring would let typos like format='asci' fall back to
        // the default response shape — confusing for the agent.
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("format".to_string(), json!("html"));
        let err = build_waves_query_params(&args).unwrap_err();
        assert!(err.to_string().contains("invalid format"),
            "expected error naming invalid format, got: {}", err);
    }

    #[test]
    fn test_build_waves_query_params_emits_width_style_with_titles() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("format".to_string(), json!("ascii"));
        args.insert("width".to_string(), json!(120));
        args.insert("style".to_string(), json!("ascii"));
        args.insert("with_titles".to_string(), json!(true));
        let params = build_waves_query_params(&args).expect("valid render args should pass");

        let lookup = |key: &str| -> String {
            params.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("expected query param {} to be present in {:?}", key, params))
        };
        assert_eq!(lookup("width"), "120");
        assert_eq!(lookup("style"), "ascii");
        assert_eq!(lookup("with_titles"), "true");
    }

    #[test]
    fn test_build_waves_query_params_rejects_zero_width() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("width".to_string(), json!(0));
        let err = build_waves_query_params(&args).unwrap_err();
        assert!(err.to_string().contains("width"),
            "expected error naming width, got: {}", err);
    }

    #[test]
    fn test_build_waves_query_params_rejects_negative_width() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("width".to_string(), json!(-5));
        let err = build_waves_query_params(&args).unwrap_err();
        assert!(err.to_string().contains("width"),
            "expected error naming width, got: {}", err);
    }

    #[test]
    fn test_build_waves_query_params_rejects_unknown_style() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("style".to_string(), json!("emoji"));
        let err = build_waves_query_params(&args).unwrap_err();
        assert!(err.to_string().contains("style"),
            "expected error naming style, got: {}", err);
    }

    #[test]
    fn test_build_waves_query_params_drops_with_titles_when_false() {
        // Symmetric with include_excluded — false is the default, so omit it
        // from the query string rather than send the noise.
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("with_titles".to_string(), json!(false));
        let params = build_waves_query_params(&args).expect("with_titles=false is valid");
        assert!(params.iter().all(|(k, _)| k != "with_titles"),
            "with_titles=false should be omitted, got: {:?}", params);
    }

    // CULTRA-1059: with_handles polish.

    #[test]
    fn test_build_waves_query_params_emits_with_handles_false() {
        // The opt-out direction. with_handles=false should pass through.
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("with_handles".to_string(), json!(false));
        let params = build_waves_query_params(&args).expect("with_handles=false is valid");
        let with_handles = params.iter().find(|(k, _)| k == "with_handles")
            .expect("with_handles=false should pass through");
        assert_eq!(with_handles.1, "false");
    }

    #[test]
    fn test_build_waves_query_params_drops_with_handles_when_true() {
        // True is the historical default. Omit from the query rather than
        // send redundant noise to the backend.
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("with_handles".to_string(), json!(true));
        let params = build_waves_query_params(&args).expect("with_handles=true is valid");
        assert!(params.iter().all(|(k, _)| k != "with_handles"),
            "with_handles=true should be omitted (default), got: {:?}", params);
    }

    // CULTRA-1069: compact_parallel passthrough.

    #[test]
    fn test_build_waves_query_params_emits_compact_parallel_when_true() {
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("compact_parallel".to_string(), json!(true));
        let params = build_waves_query_params(&args).expect("compact_parallel=true is valid");
        let cp = params.iter().find(|(k, _)| k == "compact_parallel")
            .expect("compact_parallel=true should pass through");
        assert_eq!(cp.1, "true");
    }

    #[test]
    fn test_build_waves_query_params_drops_compact_parallel_when_false() {
        // False is the historical default. Omit rather than send noise.
        let mut args = Map::new();
        args.insert("plan_id".to_string(), json!("plan-x"));
        args.insert("compact_parallel".to_string(), json!(false));
        let params = build_waves_query_params(&args).expect("compact_parallel=false is valid");
        assert!(params.iter().all(|(k, _)| k != "compact_parallel"),
            "compact_parallel=false should be omitted (default), got: {:?}", params);
    }
}
