use super::execution_waves::get_execution_waves;
use super::project_map::{merge_project_map_into, scan_project_map_tool};
use super::protocol::Tool;
use super::Server;
use super::types::{
    SessionStrategy, TaskType, TaskStatus, Priority,
    DocType, PlanStatus, DecisionStatus, EnumValues,
};
use crate::ast::{
    Parser,
    analyze_complexity,
    analyze_concurrency,
    analyze_css, find_css_rules, find_unused_selectors, css_variable_graph,
    analyze_react_component,
    find_interface_implementations,
    analyze_security,
    resolve_tailwind_classes,
    ComplexityAnalysis,
};
use crate::lsp::tools::{
    lsp_query,
    lsp_workspace_symbols,
    lsp_document_symbols,
};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::fmt;

// Embed templates at compile time
const CLAUDE_MD_TEMPLATE: &str = include_str!("../../../CLAUDE.md.TEMPLATE");
const CLAUDE_TEMPLATE_GUIDE: &str = include_str!("../../../CLAUDE_TEMPLATE_GUIDE.md");

// Embed skills at compile time
const SKILL_SECURITY_AUDIT: &str = include_str!("../../skills/security-audit.md");
const SKILL_CODE_REVIEW: &str = include_str!("../../skills/code-review.md");

/// Built-in skills that can be installed via install_skills tool
const BUILT_IN_SKILLS: &[(&str, &str)] = &[
    ("security-audit", SKILL_SECURITY_AUDIT),
    ("code-review", SKILL_CODE_REVIEW),
];

/// Get all tool definitions
pub fn get_tool_definitions() -> Vec<Tool> {
    vec![
        Tool {
            name: "load_session_state".to_string(),
            description: "Load the most recent session state for a project to resume work. Supports multiple retrieval strategies based on time decay and access patterns. Optionally includes complete plan context with smart suggestions. Response also includes a `project_map` field (CULTRA-1011) with the workspace floorplan from get_project_map for boot-up orientation; absent if the scan fails.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (e.g., 'proj-api')"
                    },
                    "strategy": {
                        "type": "string",
                        "description": "Session selection strategy: 'latest' (most recently active, default), 'relevant' (highest retrievability score based on recency + access frequency), 'merge' (combine multiple high-scoring sessions - future)",
                        "enum": ["latest", "relevant", "merge"]
                    },
                    "include_plan_context": {
                        "type": "boolean",
                        "description": "If true, includes complete plan context with sessions, tasks (with dependencies), documents, decisions, and smart suggestions for next actions. Performance: adds ~15ms. Default: false"
                    },
                    "refresh_cache": {
                        "type": "boolean",
                        "description": "If true, refreshes the plan context cache before loading (only applies when include_plan_context=true). Default: false"
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "save_session_state".to_string(),
            description: "Save current session state for future continuity".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "current_task_id": {
                        "type": "string",
                        "description": "Current task ID"
                    },
                    "current_plan_id": {
                        "type": "string",
                        "description": "Current plan ID"
                    },
                    "working_memory": {
                        "type": "object",
                        "description": "Working memory snapshot"
                    },
                    "context_snapshot": {
                        "type": "object",
                        "description": "Context snapshot"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["work", "exploration", "qa", "throwaway"],
                        "description": "Session classification (CULTRA-902). 'work' (default) for substantive work, 'exploration' for quick pokes, 'qa' for spelunking, 'throwaway' for one-off runs that should be filtered out of recent_activity."
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "create_project".to_string(),
            description: "Create a new project or update if it already exists (upsert). Projects must be created before tasks, plans, or documents can be saved.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (e.g., 'proj-myapp', 'proj-api')"
                    },
                    "name": {
                        "type": "string",
                        "description": "Human-readable project name"
                    }
                },
                "required": ["project_id", "name"]
            }),
        },
        Tool {
            name: "get_project_estimate_accuracy".to_string(),
            description: "Aggregate the actual_days / estimated_days ratio across all tasks in the project where both values are set. Returns {avg_actual_to_estimate_ratio: number|null, sample_size: int}. ratio is null when sample_size is 0 (distinguishes 'no data' from 'perfect estimates' which is 1.0). Useful for scaling future estimates: a ratio of 2.0 means the project's tasks have historically taken twice as long as estimated. Tasks with estimated_days = 0 are excluded (avoid division by zero). No status filter — cancelled tasks with set values still count. (CULTRA-1056)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "get_sessions".to_string(),
            description: "Query sessions for a project with filtering and sorting options".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of sessions to return (default: 50)"
                    },
                    "kind": {
                        "type": "string",
                        "enum": ["work", "exploration", "qa", "throwaway"],
                        "description": "Filter by session kind (CULTRA-902). Omit to include all kinds."
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "get_session".to_string(),
            description: "Retrieve a specific session by session_id with full details".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "Session identifier (required)"
                    }
                },
                "required": ["session_id"]
            }),
        },
        Tool {
            name: "get_session_code_context".to_string(),
            description: "Retrieve AST data on-demand from latest session (complements optimized load_session_state). Returns full code_context or specific file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Optional file path to retrieve specific file AST. If omitted, returns all code_context."
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "get_tasks".to_string(),
            description: "Get tasks in a project with optional structured filters (status, priority, assigned_to) and pagination (limit, offset, sort, dir). status accepts either a single value or an array of values for multi-status filtering (e.g. status=['done','cancelled']). Response shape (CULTRA-929 A1): {tasks: [...], total: N, limit: N, offset: N}. Use this when you know the exact fields you want; use search_tasks for fuzzy text queries.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "status": {
                        "oneOf": [
                            {"type": "string"},
                            {"type": "array", "items": {"type": "string"}}
                        ],
                        "description": "Filter by status. Accepts a single string ('todo', 'in_progress', 'blocked', 'done', 'cancelled') or an array of strings for multi-status filtering."
                    },
                    "priority": {
                        "type": "string",
                        "description": "Filter by priority (P0, P1, P2, P3)"
                    },
                    "assigned_to": {
                        "type": "string",
                        "description": "Filter by assignee"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Maximum tasks to return (CULTRA-929 A1). Default and cap are backend-defined."
                    },
                    "offset": {
                        "type": "integer",
                        "description": "Number of tasks to skip (CULTRA-929 A1). Combine with limit for pagination."
                    },
                    "sort": {
                        "type": "string",
                        "description": "Sort field (CULTRA-929 A1). Common values: 'updated_at', 'created_at', 'completed_at', 'priority'. Backend defines the full set."
                    },
                    "dir": {
                        "type": "string",
                        "enum": ["asc", "desc"],
                        "description": "Sort direction (CULTRA-929 A1). Default backend-defined."
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "search_tasks".to_string(),
            description: "Search tasks by keywords in title or description. Returns tasks ranked by most recently updated.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search keywords (required)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum results (default: 10, max: 50)"
                    }
                },
                "required": ["project_id", "query"]
            }),
        },
        Tool {
            name: "save_task".to_string(),
            description: "Create or update a task. Task ID is auto-generated in PROJECT-NUMBER format (e.g., CULTRA-47). Do not provide task_id. Optional estimated_days (CULTRA-1054) feeds the project's CPM critical-path computation; in_progress_started_at is auto-stamped on creation when status='in_progress'.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "plan_id": {
                        "type": "string",
                        "description": "Plan identifier (optional) - links this task to a plan"
                    },
                    "title": {
                        "type": "string",
                        "description": "Task title"
                    },
                    "description": {
                        "type": "string",
                        "description": "Task description"
                    },
                    "type": {
                        "type": "string",
                        "description": "Task type (feature, bug, chore, research, refactor, docs, test)"
                    },
                    "status": {
                        "type": "string",
                        "description": "Task status (todo, in_progress, blocked, done, cancelled)"
                    },
                    "priority": {
                        "type": "string",
                        "description": "Priority (P0, P1, P2, P3)"
                    },
                    "assigned_to": {
                        "type": "string",
                        "description": "Assignee"
                    },
                    "external_blockers": {
                        "type": "array",
                        "description": "External (non-task) blockers (CULTRA-903). Array of {reason, contact?, eta?}.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "reason": {"type": "string"},
                                "contact": {"type": "string"},
                                "eta": {"type": "string", "description": "RFC3339 timestamp"}
                            },
                            "required": ["reason"]
                        }
                    },
                    "progress_log": {
                        "type": "object",
                        "description": "Inline progress log entry written alongside the task in one call (CULTRA-900). Saves a round-trip vs. calling add_progress_log separately.",
                        "properties": {
                            "who": {"type": "string"},
                            "what": {"type": "string"}
                        },
                        "required": ["who", "what"]
                    },
                    "estimated_days": {
                        "type": "number",
                        "description": "Forward-looking effort estimate in days. Feeds CPM (critical path) and estimate-accuracy aggregates. Optional. Must be >= 0. (CULTRA-1054)"
                    },
                    "actual_days": {
                        "type": "number",
                        "description": "Realized effort in days. Normally auto-computed when status flips to 'done'; provide explicitly to override (e.g., backfilling historical data). Must be >= 0. (CULTRA-1054)"
                    },
                    "in_progress_started_at": {
                        "type": "string",
                        "description": "RFC3339 timestamp marking when work began. Normally auto-stamped on the →in_progress transition; provide explicitly when backfilling historical data. (CULTRA-1054)"
                    }
                },
                "required": ["project_id", "title"]
            }),
        },
        Tool {
            name: "get_task".to_string(),
            description: "Get a specific task by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task identifier"
                    }
                },
                "required": ["task_id"]
            }),
        },
        Tool {
            name: "get_task_chain".to_string(),
            description: "Get the full dependency chain for a task. Use direction='upstream' to find what blocks this task, direction='downstream' to find which tasks depend on this one ('who's waiting on X?'), or direction='both' (default) for full context. Use relationship_type='supersedes' (CULTRA-903) to walk the supersedes/superseded_by relationship instead of the default 'blocks' relationship. Example: get_task_chain({task_id:'X', direction:'downstream'}) answers 'who's waiting on X?'.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task identifier (required)"
                    },
                    "direction": {
                        "type": "string",
                        "enum": ["upstream", "downstream", "both"],
                        "description": "Which direction to traverse: 'upstream' (blockers), 'downstream' (blocked), or 'both' (default: both)"
                    },
                    "max_depth": {
                        "type": "number",
                        "description": "Maximum traversal depth (default: 10, max: 10)"
                    },
                    "relationship_type": {
                        "type": "string",
                        "enum": ["blocks", "supersedes"],
                        "description": "Which relationship type to walk. Default 'blocks' walks blocked_by/blocks arrays. 'supersedes' walks superseded_by/supersedes arrays (CULTRA-903)."
                    }
                },
                "required": ["task_id"]
            }),
        },
        Tool {
            name: "get_execution_waves".to_string(),
            description: "Return tasks grouped into dependency waves via topological sort (Kahn's algorithm). Wave 0 contains tasks with no in-scope blockers and can run first in parallel; wave 1 depends on wave 0, etc. Exactly one of plan_id or project_id is required. Each task carries has_external_blockers (non-task blockers from CULTRA-903) and has_external_task_deps (blockers in another plan/project or filtered out by status) summary bools — the wave represents readiness within the requested scope. If the graph contains cycles, cycle_detected=true and cycle_members lists the implicated task IDs; repair via task_dependency({action:'remove', ...}). Superseded tasks (superseded_by non-empty) are always excluded. Default status filter includes todo/in_progress/blocked/review; override via include_statuses. Pass include_excluded=true to additionally get a map of done/cancelled/superseded task IDs that were filtered out. Response always includes next_available (wave[0] task_ids), next_wave_blockers (in-progress wave[0] tasks whose completion unlocks wave 1), and components (connected components of the dependency graph; len>1 means parallel disconnected tracks). Pass format='ascii' to additionally get a server-rendered ASCII wave diagram in the `ascii` field — useful for inlining a plan view in chat or boot context.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "Plan to compute waves for. Common case: per-plan execution planning."
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project to compute waves for. Use when you want the full project DAG rather than a single plan."
                    },
                    "include_statuses": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Override default status filter. Default: ['todo','in_progress','blocked','review']. Pass include_statuses=['todo','in_progress','blocked','review','done','cancelled'] for full retrospective DAG."
                    },
                    "include_excluded": {
                        "type": "boolean",
                        "description": "When true, the response includes excluded.{done,cancelled,superseded} arrays of task IDs that were filtered out. Default: false (lean response)."
                    },
                    "format": {
                        "type": "string",
                        "enum": ["ascii"],
                        "description": "When 'ascii', the response gains an `ascii` field with a server-rendered wave diagram. Omit for the default structured response (back-compatible). (CULTRA-1049/1050)"
                    },
                    "width": {
                        "type": "integer",
                        "description": "ASCII renderer width in columns. Default: 80. Only used when format='ascii'."
                    },
                    "style": {
                        "type": "string",
                        "enum": ["unicode", "ascii"],
                        "description": "ASCII renderer glyph set. 'unicode' (default) uses ✓ ◐ ○ ⊝; 'ascii' uses [X] [/] [ ] [!] for terminals/log files where unicode mojibake is a risk."
                    },
                    "with_titles": {
                        "type": "boolean",
                        "description": "When true, the ASCII renderer includes task titles inline (one task per line). Default: false (compact wave-per-line format that fits boot context)."
                    },
                    "with_handles": {
                        "type": "boolean",
                        "description": "When false, drops the T<idx> handle prefix from compact-mode tokens. Default true preserves historical output. (CULTRA-1059)"
                    },
                    "compact_parallel": {
                        "type": "boolean",
                        "description": "When true AND every component is single-wave (N independent tasks across N components — common for plans where tasks have no inter-task edges), collapses the per-component partitioning into a single combined wave stanza. Default false preserves historical per-component output. (CULTRA-1069)"
                    }
                }
            }),
        },
        Tool {
            name: "update_task_status".to_string(),
            description: "Quickly update task status without requiring all parameters. On →in_progress, in_progress_started_at is auto-stamped (preserves existing on re-entry). On →done, actual_days is auto-computed from elapsed in-progress time; pass actual_days to override. (CULTRA-1054)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task identifier (required)"
                    },
                    "status": {
                        "type": "string",
                        "description": "New status: todo, in_progress, blocked, done, or cancelled (required)"
                    },
                    "assigned_to": {
                        "type": "string",
                        "description": "Optionally update assignee at the same time"
                    },
                    "progress_log": {
                        "type": "object",
                        "description": "Inline progress log entry written alongside the status change in one call (CULTRA-900).",
                        "properties": {
                            "who": {"type": "string"},
                            "what": {"type": "string"}
                        },
                        "required": ["who", "what"]
                    },
                    "if_version": {
                        "type": "integer",
                        "description": "Optimistic concurrency token (CULTRA-904)."
                    },
                    "estimated_days": {
                        "type": "number",
                        "description": "Set / update the forward-looking estimate alongside the status change. Must be >= 0. (CULTRA-1054)"
                    },
                    "actual_days": {
                        "type": "number",
                        "description": "Override the auto-computed actual_days. Use when the elapsed in-progress time is wrong (e.g., the task was paused outside the system). Must be >= 0. (CULTRA-1054)"
                    },
                    "in_progress_started_at": {
                        "type": "string",
                        "description": "RFC3339 timestamp. Override the auto-stamped start time, e.g., when backfilling 'I started this yesterday'. (CULTRA-1054)"
                    }
                },
                "required": ["task_id", "status"]
            }),
        },
        Tool {
            name: "update_task".to_string(),
            description: "Update an existing task's content or metadata without requiring all fields. estimated_days / actual_days / in_progress_started_at follow the same auto-side-effect rules as update_task_status when status flips. (CULTRA-1054)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task identifier (required)"
                    },
                    "title": {
                        "type": "string",
                        "description": "New title (optional)"
                    },
                    "description": {
                        "type": "string",
                        "description": "New description (optional)"
                    },
                    "plan_id": {
                        "type": "string",
                        "description": "New plan ID to link this task to (optional)"
                    },
                    "type": {
                        "type": "string",
                        "description": "New type: feature, bug, chore, research, refactor, docs, test (optional)"
                    },
                    "status": {
                        "type": "string",
                        "description": "New status (optional)"
                    },
                    "priority": {
                        "type": "string",
                        "description": "New priority: P0, P1, P2, P3 (optional)"
                    },
                    "assigned_to": {
                        "type": "string",
                        "description": "New assignee (optional)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "New tags (optional)"
                    },
                    "details": {
                        "type": "object",
                        "description": "Additional details (optional)"
                    },
                    "external_blockers": {
                        "type": "array",
                        "description": "Replace external blockers with this list (CULTRA-903). Empty array clears all. Omit to leave unchanged.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "reason": {"type": "string"},
                                "contact": {"type": "string"},
                                "eta": {"type": "string", "description": "RFC3339 timestamp"}
                            },
                            "required": ["reason"]
                        }
                    },
                    "progress_log": {
                        "type": "object",
                        "description": "Inline progress log entry written alongside the update in one call (CULTRA-900).",
                        "properties": {
                            "who": {"type": "string"},
                            "what": {"type": "string"}
                        },
                        "required": ["who", "what"]
                    },
                    "if_version": {
                        "type": "integer",
                        "description": "Optimistic concurrency token (CULTRA-904). Include the version you read; if the row has changed since, the update fails with a version conflict and the response includes the current version for retry."
                    },
                    "estimated_days": {
                        "type": "number",
                        "description": "Set / update the forward-looking estimate. Must be >= 0. (CULTRA-1054)"
                    },
                    "actual_days": {
                        "type": "number",
                        "description": "Override the auto-computed actual_days. Use when the elapsed in-progress time is wrong (e.g., the task was paused outside the system). Must be >= 0. (CULTRA-1054)"
                    },
                    "in_progress_started_at": {
                        "type": "string",
                        "description": "RFC3339 timestamp. Override the auto-stamped start time, e.g., when backfilling historical data. (CULTRA-1054)"
                    }
                },
                "required": ["task_id"]
            }),
        },
        Tool {
            name: "task_dependency".to_string(),
            description: "Add or remove a dependency relationship between tasks. Default type='blocks' updates blocks/blocked_by arrays. type='supersedes' (CULTRA-903) updates supersedes/superseded_by arrays for the 'this task replaces that one' relationship.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["add", "remove"],
                        "description": "Whether to add or remove the dependency (required)"
                    },
                    "task_id": {
                        "type": "string",
                        "description": "Task that will be blocked/unblocked (required)"
                    },
                    "depends_on": {
                        "type": "string",
                        "description": "Task that must be completed first / to remove from dependencies (required)"
                    },
                    "type": {
                        "type": "string",
                        "enum": ["blocks", "supersedes"],
                        "description": "Relationship type. Default 'blocks'. 'supersedes' marks task_id as superseded by depends_on (CULTRA-903)."
                    }
                },
                "required": ["action", "task_id", "depends_on"]
            }),
        },
        Tool {
            name: "add_progress_log".to_string(),
            description: "Add a progress log entry to a task".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "task_id": {
                        "type": "string",
                        "description": "Task identifier"
                    },
                    "who": {
                        "type": "string",
                        "description": "Who made the progress"
                    },
                    "what": {
                        "type": "string",
                        "description": "What progress was made"
                    }
                },
                "required": ["task_id", "who", "what"]
            }),
        },
        Tool {
            name: "save_document".to_string(),
            description: "Create or update a markdown document".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "description": "Document identifier (required)"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "title": {
                        "type": "string",
                        "description": "Document title (required)"
                    },
                    "content": {
                        "type": "string",
                        "description": "Markdown content (required unless file_path is provided)"
                    },
                    "content_file": {
                        "type": "string",
                        "description": "Absolute path to a local markdown file to read as content. Overrides content if both are provided."
                    },
                    "doc_type": {
                        "type": "string",
                        "description": "Document type: guide, test_report, decision, architecture, etc. (default: guide)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"}
                    },
                    "linked_tasks": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Task IDs to link this document to"
                    },
                    "created_by": {
                        "type": "string",
                        "description": "Author (default: claude)"
                    }
                },
                "required": ["document_id", "project_id", "title", "content"]
            }),
        },
        Tool {
            name: "get_documents".to_string(),
            description: "Query documents with optional filters".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "doc_type": {
                        "type": "string",
                        "description": "Filter by document type"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter by tags"
                    },
                    "linked_task_id": {
                        "type": "string",
                        "description": "Filter by linked task"
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "get_document".to_string(),
            description: "Get a specific document by ID".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "description": "Document identifier (required)"
                    }
                },
                "required": ["document_id"]
            }),
        },
        Tool {
            name: "update_document".to_string(),
            description: "Update an existing document's content or metadata without requiring all fields".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "description": "Document identifier (required)"
                    },
                    "title": {
                        "type": "string",
                        "description": "New title (optional)"
                    },
                    "content": {
                        "type": "string",
                        "description": "New markdown content (optional)"
                    },
                    "content_file": {
                        "type": "string",
                        "description": "Absolute path to a local markdown file to read as content. Overrides content if both are provided."
                    },
                    "doc_type": {
                        "type": "string",
                        "description": "New document type (optional)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "New tags (optional)"
                    },
                    "if_version": {
                        "type": "integer",
                        "description": "Optimistic concurrency token (CULTRA-904). Include the version you read; mismatched updates fail with a version conflict."
                    }
                },
                "required": ["document_id"]
            }),
        },
        Tool {
            name: "link_document".to_string(),
            description: "Link a document to tasks and/or plans (CULTRA-1065). At least one of task_ids, plan_id, or plan_ids must be provided. Plan linking enables attaching design docs to draft plans before tasks are filed — closes the gap that breaks the research-first workflow.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "document_id": {
                        "type": "string",
                        "description": "Document identifier (required)"
                    },
                    "task_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Task IDs to link (optional if plan_id/plan_ids provided)"
                    },
                    "plan_id": {
                        "type": "string",
                        "description": "Single plan ID to link (optional if task_ids/plan_ids provided). CULTRA-1065."
                    },
                    "plan_ids": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Plan IDs to link (optional if task_ids/plan_id provided). CULTRA-1065."
                    }
                },
                "required": ["document_id"]
            }),
        },
        Tool {
            name: "save_plan".to_string(),
            description: "Create or update a plan".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "Plan identifier"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "title": {
                        "type": "string",
                        "description": "Plan title"
                    },
                    "content": {
                        "type": "object",
                        "description": "Plan content"
                    },
                    "status": {
                        "type": "string",
                        "description": "Status (draft, in_progress, completed)"
                    },
                    "priority": {
                        "type": "string",
                        "description": "Priority (P0, P1, P2, P3)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"}
                    },
                    "problem": {
                        "type": "string",
                        "description": "Engine V3: What need or issue this plan addresses (optional)"
                    },
                    "goal": {
                        "type": "string",
                        "description": "Engine V3: What you're trying to achieve (optional)"
                    },
                    "approach": {
                        "type": "string",
                        "description": "Engine V3: Technical strategy or methodology (optional)"
                    },
                    "estimated_duration": {
                        "type": "string",
                        "description": "Engine V3: Time estimate, e.g. '2-3 hours' (optional)"
                    }
                },
                "required": ["plan_id", "project_id", "title"]
            }),
        },
        Tool {
            name: "get_plans".to_string(),
            description: "Query plans for a project".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter by status"
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "get_plan".to_string(),
            description: "Get plan overview, full details, or a rendered ASCII wave diagram. Default (detail=\"status\"): tasks with dependencies, progress summary, next available tasks. detail=\"full\": Engine V3 content (problem, goal, approach), all tasks with progress logs, linked documents. detail=\"ascii\" (CULTRA-1049): server-rendered wave diagram of the plan's execution structure (waves + next_available + next_wave_blockers + components + ascii string) — useful for orienting on plan progress at a glance, especially at session boot. group_by=\"tag\" (CULTRA-1062, only valid with detail=\"status\"): adds a task_groups field to the response — a tag → [task_ids] map for phase-grouped views (multi-tag tasks appear in each group; untagged tasks land in an 'untagged' bucket).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "Plan identifier (required)"
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["status", "full", "ascii"],
                        "description": "Detail level: 'status' (default) for overview, 'full' for Engine V3 details with progress logs and linked documents, 'ascii' for a rendered wave diagram (CULTRA-1049)."
                    },
                    "width": {
                        "type": "integer",
                        "description": "ASCII renderer width in columns. Default: 80. Only used when detail='ascii'."
                    },
                    "style": {
                        "type": "string",
                        "enum": ["unicode", "ascii"],
                        "description": "ASCII renderer glyph set. 'unicode' (default) uses ✓ ◐ ○ ⊝; 'ascii' uses [X] [/] [ ] [!]."
                    },
                    "with_titles": {
                        "type": "boolean",
                        "description": "When true, ASCII renderer includes task titles inline. Default: false."
                    },
                    "with_handles": {
                        "type": "boolean",
                        "description": "When false, drops the T<idx> handle prefix from compact-mode tokens (e.g. '(○ CULTRA-1)' instead of 'T0(○ CULTRA-1)'). Default true preserves historical output. (CULTRA-1059)"
                    },
                    "compact_parallel": {
                        "type": "boolean",
                        "description": "When true AND every component is single-wave, collapses per-component partitioning into one combined stanza. Default false preserves historical output. Only valid with detail='ascii'. (CULTRA-1069)"
                    },
                    "group_by": {
                        "type": "string",
                        "enum": ["tag"],
                        "description": "Group plan tasks by a category. Currently only 'tag' is supported — adds task_groups: {tag: [task_ids]} to the response (multi-tag tasks appear in each of their groups; untagged tasks land in an 'untagged' sentinel). Only valid with detail='status'. (CULTRA-1062)"
                    }
                },
                "required": ["plan_id"]
            }),
        },
        Tool {
            name: "save_decision".to_string(),
            description: "Create or update a decision".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "decision_id": {
                        "type": "string",
                        "description": "Decision identifier"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "title": {
                        "type": "string",
                        "description": "Decision title"
                    },
                    "content": {
                        "type": "object",
                        "description": "Decision content"
                    },
                    "status": {
                        "type": "string",
                        "description": "Status (proposed, accepted, deprecated, superseded)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"}
                    }
                },
                "required": ["decision_id", "project_id", "title"]
            }),
        },
        Tool {
            name: "get_decisions".to_string(),
            description: "Query decisions for a project".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter by status"
                    }
                },
                "required": ["project_id"]
            }),
        },
        // AST Tools
        Tool {
            name: "parse_file_ast".to_string(),
            description: "Parse a code file and extract AST metadata (symbols, functions, calls, imports). Returns semantic information about the code structure. Set with_callers=true to enrich each exported function/method with up to 10 cross-file callers via LSP references (CULTRA-906).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the code file to parse (.go, .ts, .tsx, .js, .jsx, .py, .rs, .php, .tf)"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (optional, defaults to 'proj-cultra')"
                    },
                    "with_callers": {
                        "type": "boolean",
                        "description": "If true, attach a 'callers' list to each exported function/method symbol via LSP textDocument/references (CULTRA-906). Best-effort: degrades gracefully if LSP is unavailable. Per-symbol caller lookups can be slow on large workspaces."
                    },
                    "preview_lines": {
                        "type": "number",
                        "description": "CULTRA-994: include the first N lines of each symbol's source body as a 'preview' array. Useful for orientation without a separate Read call. Default: omitted (no preview)."
                    }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "diff_file_ast".to_string(),
            description: "Structural AST diff between two git revisions of the same file (CULTRA-909). Compares symbols by name and reports added, removed, and signature-changed functions/types. Use this for code review questions like 'what symbols did this branch add?' that raw text diff struggles to answer cleanly. base_ref is required; head_ref defaults to HEAD.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file (must be inside the workspace git repo)"
                    },
                    "base_ref": {
                        "type": "string",
                        "description": "Git ref for the 'old' version (e.g., 'main', 'HEAD~5', commit SHA)"
                    },
                    "head_ref": {
                        "type": "string",
                        "description": "Git ref for the 'new' version (default: 'HEAD')"
                    }
                },
                "required": ["file_path", "base_ref"]
            }),
        },
        Tool {
            name: "analyze_changes".to_string(),
            description: "Run an analyzer (CULTRA-908) over only the files that changed between a git ref and the working tree. Thin wrapper around analyze_files. CULTRA-956: include_working_tree defaults to true, so 'analyze what my branch introduced' covers uncommitted work by default. Pass include_working_tree=false to scope strictly to committed changes between {since} and HEAD. The empty-result message distinguishes 'no changes found' from 'language filter rejected files' so a misconfiguration is debuggable.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "since": {
                        "type": "string",
                        "description": "Git ref to diff against (e.g., 'main', 'HEAD~5', commit SHA). With include_working_tree=true (default) this is `git diff <since>` (working-tree vs ref). With include_working_tree=false this is `git diff <since> HEAD` (committed-only)."
                    },
                    "analyzer": {
                        "type": "string",
                        "enum": ["concurrency", "react", "css", "css_variables", "security", "complexity"],
                        "description": "Analyzer to run on each changed file"
                    },
                    "include_working_tree": {
                        "type": "boolean",
                        "description": "If true (CULTRA-956 default), uncommitted working-tree changes are included in the diff. Pass false to restrict to committed diffs only."
                    },
                    "min_cyclomatic": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Forwarded to analyze_files."
                    },
                    "min_cognitive": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Forwarded to analyze_files."
                    },
                    "top_n": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Per-file top-N. Forwarded to analyze_files."
                    }
                },
                "required": ["since", "analyzer"]
            }),
        },
        Tool {
            name: "find_dead_code".to_string(),
            description: "Find exported functions/methods in a file that are never referenced from anywhere in the workspace (CULTRA-907). Uses LSP textDocument/references for each exported callable. Caveats: dynamic dispatch, reflection, build tags, and test-only exports are NOT detected and may produce false positives. Requires a running language server. The response includes an lsp_index_status field ('warm'|'cold'|'unknown') so callers can detect a cold LSP index without digging through caveats (CULTRA-947). Set require_warm_index=true to fail fast rather than receive best-effort results when the index isn't ready.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the source file to scan"
                    },
                    "require_warm_index": {
                        "type": "boolean",
                        "description": "If true, return an error when the LSP index appears cold (every checked symbol returns zero references) instead of producing best-effort 'low' confidence findings. Defaults to false for backward compatibility."
                    },
                    "warmup": {
                        "type": "boolean",
                        "description": "CULTRA-950: if true, run a per-language warmup command (e.g. `cargo check` for Rust, `go build ./...` for Go) before querying LSP, so the index is populated. Cached per session and invalidated when source files are touched. First call in a fresh session pays the cold-start cost (typically 5-30s); subsequent calls are instant. Languages with no warmup command return status='skipped' instead of erroring. Default false."
                    }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "find_references".to_string(),
            description: "Semantic call-site lookup for a symbol (CULTRA-948). Uses LSP textDocument/references to find every usage of `symbol` declared in `file_path`, then classifies each hit with a role field: `definition` | `call` | `type_use` | `doc` | `unknown`. Use this instead of Grep during refactors when you need to know whether a hit is a function call, a type reference, or prose in a doc comment. Inherits the CULTRA-947 cold-index guard: the response includes an `lsp_index_status` field, and cold results (no cross-site references) surface a top-level `warning`. Pass `require_warm_index=true` to fail fast on a cold index instead of getting a best-effort empty result.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "symbol": {
                        "type": "string",
                        "description": "Symbol name to find references for (e.g. 'MyFunction', 'compose_from_diff_screens'). Must be declared in file_path."
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file that declares the symbol. Used as the LSP anchor position."
                    },
                    "include_declaration": {
                        "type": "boolean",
                        "description": "If true, the declaration site is included in results as role='definition'. Default false — most refactors only want call sites."
                    },
                    "require_warm_index": {
                        "type": "boolean",
                        "description": "If true, return an error when the LSP index appears cold (references list is empty or declaration-only). Default false."
                    },
                    "warmup": {
                        "type": "boolean",
                        "description": "CULTRA-950: if true, run a per-language warmup command before querying LSP. See find_dead_code's warmup docs for details. Cached per session and mtime-invalidated. Default false."
                    }
                },
                "required": ["symbol", "file_path"]
            }),
        },
        Tool {
            name: "analyze_symbol".to_string(),
            description: "Single-function metrics + optional delta-against-baseline mode (CULTRA-949). Filters analyze_file(complexity)'s output to one symbol so a refactor loop can run this after every edit and see cyclomatic/cognitive/lines move by {-3, +0, +1} instead of re-reading a ~100-function JSON blob. Pass delta_against={cyclomatic, cognitive, lines[, rating]} to turn the response into live deltas with `prev`/`now`/`delta` per metric. v1 supports analyzer='complexity' only.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the source file containing the symbol."
                    },
                    "symbol": {
                        "type": "string",
                        "description": "Function or method name to analyze. If the file has multiple functions with the same name (overloads / different impls), the first match is returned with a top-level warning."
                    },
                    "analyzer": {
                        "type": "string",
                        "enum": ["complexity"],
                        "description": "Analyzer to run. v1 only supports 'complexity'. Default: 'complexity'."
                    },
                    "delta_against": {
                        "type": "object",
                        "description": "Optional inline baseline. When present, each metric is returned as {prev, now, delta} instead of a scalar. Keys: cyclomatic (int), cognitive (int), lines (int), rating (string, optional). Missing keys fall back to the scalar form for that metric.",
                        "properties": {
                            "cyclomatic": {"type": "integer"},
                            "cognitive": {"type": "integer"},
                            "lines": {"type": "integer"},
                            "rating": {"type": "string"}
                        }
                    }
                },
                "required": ["file_path", "symbol"]
            }),
        },
        Tool {
            name: "analyze_files".to_string(),
            description: "Bulk variant of analyze_file (CULTRA-905). Runs the same analyzer against many files in parallel and returns one entry per input file. Per-file failures are isolated — one bad path doesn't abort the batch. Use this instead of wrapping individual analyze_file calls in `batch` for whole-package or whole-repo audits. CULTRA-1066: complexity analyzer accepts min_cyclomatic / min_cognitive / top_n filters to drop low-signal functions before serialization (per file, not global) — large token-economic win on big sweeps.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "analyzer": {
                        "type": "string",
                        "enum": ["concurrency", "react", "css", "css_variables", "security", "complexity"],
                        "description": "Analyzer type (required)"
                    },
                    "file_paths": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Absolute paths to analyze. Capped at 500 entries per call. Order is preserved in the result."
                    },
                    "min_cyclomatic": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Drop functions with cyclomatic complexity below N. Summary aggregates are preserved (file-level metrics shouldn't change with view)."
                    },
                    "min_cognitive": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Drop functions with cognitive complexity below N."
                    },
                    "top_n": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Return only the top-N highest-cyclomatic functions per file (functions are pre-sorted, so this just truncates)."
                    }
                },
                "required": ["analyzer", "file_paths"]
            }),
        },
        Tool {
            name: "analyze_file".to_string(),
            description: "Analyze a source file. analyzer=\"concurrency\": Go concurrency patterns (goroutines, channels, mutex, race conditions). analyzer=\"react\": React component structure (props, hooks, state, children). analyzer=\"css\": CSS structural metadata (selectors, specificity, variables, media queries). analyzer=\"css_variables\": CSS custom property dependency graph (var() chains, cycles, unresolved refs). analyzer=\"security\": Security vulnerability scan (SQL injection, XSS, command injection, hardcoded secrets, insecure crypto, SSRF, Terraform misconfigs — multi-language). analyzer=\"complexity\": Cyclomatic and cognitive complexity per function/resource block with ratings (multi-language incl. Terraform). CULTRA-1066: complexity analyzer accepts min_cyclomatic / min_cognitive / top_n filters to drop low-signal functions before serialization.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "analyzer": {
                        "type": "string",
                        "enum": ["concurrency", "react", "css", "css_variables", "security", "complexity"],
                        "description": "Analyzer type (required)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to analyze"
                    },
                    "min_cyclomatic": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Drop functions with cyclomatic complexity below N. Summary aggregates preserved."
                    },
                    "min_cognitive": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Drop functions with cognitive complexity below N."
                    },
                    "top_n": {
                        "type": "integer",
                        "minimum": 1,
                        "description": "CULTRA-1066: complexity analyzer only. Return only the top-N highest-cyclomatic functions (pre-sorted)."
                    }
                },
                "required": ["analyzer", "file_path"]
            }),
        },
        Tool {
            name: "find_interface_implementations".to_string(),
            description: "Find Go types that implement specified interfaces. Returns interface definitions with method signatures, and all types that fully or partially implement each interface. Detects missing methods for incomplete implementations. Useful for refactoring, finding implementers, and validating interface satisfaction.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the Go file to analyze"
                    },
                    "interface_name": {
                        "type": "string",
                        "description": "(Optional) Filter results to show only implementations of this specific interface"
                    }
                },
                "required": ["file_path"]
            }),
        },
        // CULTRA-995: contextual search — grep + AST annotation
        Tool {
            name: "contextual_search".to_string(),
            description: "Search for a text pattern and annotate each match with the AST symbol that contains it (CULTRA-995). Combines grep-style text search with parse_file_ast-level context. Each match includes the file, line, matched text, and the containing function/class/method name and line range. Use this instead of Grep when you need to know 'which function contains this pattern' without a separate parse_file_ast call per file.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "pattern": {
                        "type": "string",
                        "description": "Text pattern to search for (passed to ripgrep)"
                    },
                    "path": {
                        "type": "string",
                        "description": "Directory or file to search in. Defaults to workspace root. Must be within workspace."
                    },
                    "glob": {
                        "type": "string",
                        "description": "File glob filter (e.g., '*.py', '*.rs'). Passed to ripgrep --glob."
                    },
                    "max_results": {
                        "type": "number",
                        "description": "Maximum matches to return. Default: 100."
                    }
                },
                "required": ["pattern"]
            }),
        },
        // CULTRA-996: project manifest reader
        Tool {
            name: "project_info".to_string(),
            description: "Read a project's manifest file and return structured metadata (CULTRA-996). Supports Cargo.toml, pyproject.toml, package.json, go.mod, composer.json. Returns language, dependencies, dev dependencies, version constraints, package manager, and LSP server availability. Use this instead of manually reading manifest files.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Path to the project directory (must contain a manifest file). Defaults to workspace root."
                    }
                },
                "required": []
            }),
        },
        // CULTRA-1009: workspace floorplan for boot-up orientation
        Tool {
            name: "get_project_map".to_string(),
            description: "Return a deterministic floorplan of the workspace: top-level directories classified by language/framework from their manifest files. Nested repos (with their own .git) are flagged is_own_repo=true and pruned to just a boundary marker — call get_project_map with their path for details. Submodules/worktrees are flagged submodule=true but kept with full manifest info (they're part of parent history). Use this at session boot to orient: what's here, what's mine to edit, what's somebody else's.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "path": {
                        "type": "string",
                        "description": "Optional workspace root. Defaults to the server's workspace_root. Must be within the workspace root."
                    }
                }
            }),
        },
        // CSS Analysis Tools
        Tool {
            name: "find_css_rules".to_string(),
            description: "Search for CSS rules matching a selector pattern. Returns full rule blocks with properties, line numbers, and specificity. Pattern is matched as case-insensitive substring against selectors.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the CSS file to search (.css)"
                    },
                    "pattern": {
                        "type": "string",
                        "description": "Selector pattern to search for (substring match, e.g. '.kb-sidebar' or 'btn')"
                    }
                },
                "required": ["file_path", "pattern"]
            }),
        },
        Tool {
            name: "find_unused_selectors".to_string(),
            description: "Cross-reference CSS selectors against component files to find orphaned/unused CSS rules. Scans a directory for .tsx/.ts/.jsx/.js/.html files and checks if class names from CSS selectors appear in any of them. Returns selectors with no matches found.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "css_path": {
                        "type": "string",
                        "description": "Absolute path to the CSS file to check"
                    },
                    "component_dir": {
                        "type": "string",
                        "description": "Absolute path to directory containing component files to scan (.tsx, .ts, .jsx, .js, .html)"
                    }
                },
                "required": ["css_path", "component_dir"]
            }),
        },
        // CSS Analysis Tools V2
        Tool {
            name: "resolve_tailwind_classes".to_string(),
            description: "Resolve Tailwind CSS utility classes to their CSS property declarations. Supports all utility categories (layout, spacing, colors, typography, borders, effects, transforms). Optionally reads @theme block from a CSS file for custom theme resolution (Tailwind v4).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "classes": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Array of Tailwind class names to resolve (e.g. [\"flex\", \"p-4\", \"bg-red-500\", \"hover:text-white\"])"
                    },
                    "css_path": {
                        "type": "string",
                        "description": "Optional absolute path to a CSS file containing @theme block for custom theme values (Tailwind v4)"
                    }
                },
                "required": ["classes"]
            }),
        },
        // Engine V2 Intelligence Tools
        Tool {
            name: "search_code_context".to_string(),
            description: "Search through AST metadata from the latest session. Find symbols by name, type, file, scope, receiver, or call relationships.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "symbol_name": {
                        "type": "string",
                        "description": "Symbol name to search for (partial match, case-insensitive)"
                    },
                    "symbol_type": {
                        "type": "string",
                        "description": "Symbol type: function, method, type, class, interface"
                    },
                    "calls": {
                        "type": "string",
                        "description": "Find symbols that call this function (forward lookup)"
                    },
                    "receiver": {
                        "type": "string",
                        "description": "Method receiver (Go only, e.g., '*API', '*Server')"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Symbol scope: exported, unexported, public, private"
                    },
                    "file_pattern": {
                        "type": "string",
                        "description": "File path pattern to filter results (e.g., 'handlers.go', 'internal/api/')"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of results (default 50)"
                    }
                },
                "required": ["project_id"]
            }),
        },
        Tool {
            name: "read_symbol_lines".to_string(),
            description: "Read specific line ranges from a file based on symbol location. Complements search_code_context by allowing immediate code reading after search.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "location": {
                        "type": "string",
                        "description": "Location string in format 'file:line' or 'file:start-end' (e.g., 'parser.go:57-130' or 'parser.go:57')"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file (alternative to location string)"
                    },
                    "start_line": {
                        "type": "number",
                        "description": "Starting line number (1-indexed). Required if using file_path instead of location."
                    },
                    "end_line": {
                        "type": "number",
                        "description": "Ending line number (1-indexed). Optional, defaults to start_line if omitted."
                    }
                }
            }),
        },
        Tool {
            name: "init_vector_db".to_string(),
            description: "Initialize Qdrant vector database collection for semantic search. Creates collection with proper dimensions and distance metric.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "recreate": {
                        "type": "boolean",
                        "description": "If true, delete existing collection and recreate. Use with caution! (default: false)"
                    }
                }
            }),
        },
        Tool {
            name: "query_context".to_string(),
            description: "Semantic search for relevant documents using vector embeddings. Returns documents similar to your query based on meaning, not just keywords. Supports graph enrichment to find related entities.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Natural language query (e.g., 'How does task dependency tracking work?') (required)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of results to return (default: 5, max: 20)"
                    },
                    "min_score": {
                        "type": "number",
                        "description": "Minimum similarity score (0.0-1.0). Lower = more permissive (default: 0.7)"
                    },
                    "include_graph": {
                        "type": "boolean",
                        "description": "Enable graph enrichment to find related entities via graph traversal (default: false)"
                    },
                    "graph_depth": {
                        "type": "number",
                        "description": "Maximum graph traversal depth when include_graph is true (default: 2, max: 5)"
                    },
                    "use_retrievability": {
                        "type": "boolean",
                        "description": "Boost frequently accessed entities when include_graph is true (default: false)"
                    }
                },
                "required": ["project_id", "query"]
            }),
        },
        Tool {
            name: "add_graph_edge".to_string(),
            description: "Create a relationship edge between two entities in the graph (Engine V2)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "from_type": {
                        "type": "string",
                        "description": "Source entity type (session, task, plan, document, decision)"
                    },
                    "from_id": {
                        "type": "string",
                        "description": "Source entity ID"
                    },
                    "to_type": {
                        "type": "string",
                        "description": "Target entity type"
                    },
                    "to_id": {
                        "type": "string",
                        "description": "Target entity ID"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Edge type from allowlist (belongs_to, depends_on, supports, etc.)"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "strength": {
                        "type": "number",
                        "description": "Relationship strength [0.0-1.0] (optional, uses default)"
                    },
                    "metadata": {
                        "type": "object",
                        "description": "Provenance metadata (created_by, rule_id, etc.)"
                    }
                },
                "required": ["from_type", "from_id", "to_type", "to_id", "edge_type", "project_id"]
            }),
        },
        Tool {
            name: "query_graph".to_string(),
            description: "Query the entity graph to find connected entities (Engine V2)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "entity_type": {
                        "type": "string",
                        "description": "Entity type to query from"
                    },
                    "entity_id": {
                        "type": "string",
                        "description": "Entity ID to query from"
                    },
                    "edge_type": {
                        "type": "string",
                        "description": "Filter by edge type (optional)"
                    },
                    "direction": {
                        "type": "string",
                        "description": "Traversal direction: 'forward', 'reverse', or 'both' (default: forward). 'both' queries both directions and merges results.",
                        "enum": ["forward", "reverse", "both"]
                    },
                    "depth": {
                        "type": "number",
                        "description": "Max traversal depth (default: 1, max: 5)"
                    }
                },
                "required": ["project_id", "entity_type", "entity_id"]
            }),
        },
        Tool {
            name: "get_graph_neighbors".to_string(),
            description: "Get immediate neighbors of an entity in the graph (Engine V2)".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "entity_type": {
                        "type": "string",
                        "description": "Entity type"
                    },
                    "entity_id": {
                        "type": "string",
                        "description": "Entity ID"
                    },
                    "edge_classes": {
                        "type": "string",
                        "description": "Comma-separated edge classes (STRUCTURAL,EVIDENCE,HEURISTIC). Default: STRUCTURAL,EVIDENCE"
                    }
                },
                "required": ["project_id", "entity_type", "entity_id"]
            }),
        },
        // LSP Tools
        Tool {
            name: "lsp".to_string(),
            description: "LSP position query. action=\"references\": find all references. action=\"definition\": jump to definition. action=\"hover\": get type info and docs. action=\"implementation\": find trait/interface implementations (CULTRA-966). CULTRA-955: workspace_root is now resolved by walking up from file_path to the language manifest (Cargo.toml, go.mod, tsconfig.json) instead of defaulting to the MCP cwd. Response includes lsp_index_status='warm'|'cold'|'unknown' and a top-level warning when cold so callers can distinguish a real empty result from a not-yet-indexed workspace. Pass require_warm_index=true to fail fast on cold.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["references", "definition", "hover", "implementation"],
                        "description": "LSP action to perform (required)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the source file"
                    },
                    "line": {
                        "type": "number",
                        "description": "Line number (0-indexed)"
                    },
                    "character": {
                        "type": "number",
                        "description": "Character offset (0-indexed)"
                    },
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional workspace root override. CULTRA-955: when omitted, the workspace root is resolved by walking up from file_path to the language's manifest file (Cargo.toml / go.mod / tsconfig.json). Only override this if the walk-up resolves to the wrong project."
                    },
                    "require_warm_index": {
                        "type": "boolean",
                        "description": "If true (CULTRA-955), return an error when the LSP returns no useful results — usually means the index isn't ready yet. Default false: returns best-effort results with lsp_index_status='cold' and a warning."
                    },
                    "warmup": {
                        "type": "boolean",
                        "description": "CULTRA-963: if true, run a per-language warmup command (e.g. `cargo check` for Rust, `go build ./...` for Go) before querying LSP, so the index is fully populated for positional queries (hover, definition) that need semantic analysis beyond declaration-level info. Cached per session and invalidated when source files are touched. Default false."
                    }
                },
                "required": ["action", "file_path", "line", "character"]
            }),
        },
        Tool {
            name: "lsp_workspace_symbols".to_string(),
            description: "Search for symbols across the entire workspace using LSP. CULTRA-955: pass either workspace_root explicitly OR a file_path hint (any file inside the desired workspace) so the resolver can walk up to the language manifest. Without either, falls back to the MCP cwd which is usually wrong in nested-crate layouts. Response includes lsp_index_status and a cold-index warning when zero symbols come back.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Symbol name to search for"
                    },
                    "language": {
                        "type": "string",
                        "description": "Programming language (e.g., 'go', 'rust', 'typescript')"
                    },
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional explicit workspace root. Wins over file_path hint."
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Optional hint: absolute path to any file inside the desired workspace. CULTRA-955: when given, the workspace root is resolved by walking up from this path to the language's manifest. Recommended over relying on the manager default."
                    },
                    "require_warm_index": {
                        "type": "boolean",
                        "description": "If true (CULTRA-955), return an error when zero symbols come back. Default false: returns the empty result with lsp_index_status='cold' and a warning."
                    },
                    "warmup": {
                        "type": "boolean",
                        "description": "CULTRA-963: if true, run a per-language warmup command before querying LSP. Uses file_path hint for warmup target. Cached per session. Default false."
                    }
                },
                "required": ["query", "language"]
            }),
        },
        Tool {
            name: "lsp_document_symbols".to_string(),
            description: "List all symbols in a document using LSP. CULTRA-955: workspace_root is now resolved by walking up from file_path to the language manifest. Response includes lsp_index_status and a top-level warning when zero symbols come back, so a confidently-empty result (file genuinely has no symbols) is distinguishable from a cold index (workspace not yet indexed).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the source file"
                    },
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional workspace root override. CULTRA-955: defaults to the manifest dir found by walking up from file_path."
                    },
                    "require_warm_index": {
                        "type": "boolean",
                        "description": "If true (CULTRA-955), return an error when zero symbols come back. Default false."
                    },
                    "warmup": {
                        "type": "boolean",
                        "description": "CULTRA-963: if true, run a per-language warmup command before querying LSP. Cached per session. Default false."
                    },
                    "max_results": {
                        "type": "number",
                        "description": "CULTRA-971: maximum number of symbols to return. Useful for large files (e.g. api.ts with 90+ methods). When set, response includes total, offset, and max_results fields for pagination."
                    },
                    "offset": {
                        "type": "number",
                        "description": "CULTRA-971: skip the first N symbols before applying max_results. Default 0. Use with max_results to paginate through large symbol lists."
                    }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "batch".to_string(),
            description: "Execute multiple tool calls in one request. Operations run sequentially; each result is independent. Cannot call batch recursively.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "operations": {
                        "type": "array",
                        "description": "Tool calls to execute. Max 20.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "tool": { "type": "string", "description": "Tool name" },
                                "args": { "type": "object", "description": "Tool arguments" }
                            },
                            "required": ["tool", "args"]
                        },
                        "minItems": 1,
                        "maxItems": 20
                    }
                },
                "required": ["operations"]
            }),
        },
        // Activity feed
        Tool {
            name: "recent_activity".to_string(),
            description: "Get recent activity across a project — tasks updated, documents changed, decisions made, sessions. Answers 'what happened since I was last here?' in one call.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (optional if CLAUDE.md has Project line)"
                    },
                    "hours": {
                        "type": "number",
                        "description": "Look back N hours (default: 24, max: 168)"
                    }
                }
            }),
        },
        // Knowledge Base Tools
        Tool {
            name: "kb_ask".to_string(),
            description: "Query the knowledge base using RAG (Retrieval-Augmented Generation). Returns an AI-generated answer with source citations from indexed KB documents.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Natural language question to ask the knowledge base (required)"
                    },
                    "space_key": {
                        "type": "string",
                        "description": "Optional KB space key to limit search scope"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum number of source chunks to retrieve (default: 5)"
                    }
                },
                "required": ["query"]
            }),
        },
        // Unified Search
        Tool {
            name: "unified_search".to_string(),
            description: "Search across all entities in a project (tasks, documents, plans, decisions, sessions). Returns ranked results from all entity types.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (required)"
                    },
                    "query": {
                        "type": "string",
                        "description": "Search query (required)"
                    },
                    "entity_types": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "Filter by entity types: task, document, plan, decision, session (optional, default: all)"
                    },
                    "limit": {
                        "type": "number",
                        "description": "Maximum results (default: 20, max: 50)"
                    }
                },
                "required": ["project_id", "query"]
            }),
        },
        Tool {
            name: "install_skills".to_string(),
            description: "Install Cultra's built-in Claude Code skills into .claude/skills/ in the current workspace. Skills provide specialized modes like security auditing and code review. Idempotent — skips existing files unless force=true.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "force": {
                        "type": "boolean",
                        "description": "Overwrite existing skill files (default: false)"
                    },
                    "list": {
                        "type": "boolean",
                        "description": "Just list available skills without installing (default: false)"
                    }
                }
            }),
        },
        Tool {
            name: "get_template".to_string(),
            description: "Get a built-in Cultra template. name=\"claude_md\": CLAUDE.md template for new projects. name=\"template_guide\": setup guide explaining how to customize the template.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "enum": ["claude_md", "template_guide"],
                        "description": "Template name (required)"
                    }
                },
                "required": ["name"]
            }),
        },
    ]
}

// ========== Generic Tool Helpers ==========

/// Resolve project_id: use provided value, fall back to server default, or error
fn resolve_project_id(server: &Server, args: &mut Map<String, Value>) -> Result<()> {
    if let Some(Value::String(pid)) = args.get("project_id") {
        if !pid.is_empty() {
            return Ok(()); // Already provided
        }
    }
    // Fall back to default
    if let Some(ref default_pid) = server.default_project_id {
        args.insert("project_id".to_string(), Value::String(default_pid.clone()));
        Ok(())
    } else {
        Err(anyhow!("Missing required parameter: project_id (no default detected from CLAUDE.md)"))
    }
}

// ========== Helper Functions ==========

/// Get human-readable type name from a JSON Value
fn get_value_type_name(value: &Value) -> &'static str {
    match value {
        Value::String(_) => "string",
        Value::Number(_) => "number",
        Value::Bool(_) => "boolean",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
        Value::Null => "null",
    }
}

/// Validate that a file path exists, is readable, and is within the workspace
fn validate_file_exists(file_path: &str, workspace_root: &std::path::Path) -> Result<()> {
    let path = std::path::Path::new(file_path);

    // Canonicalize to resolve symlinks and ../ traversal
    let canonical = path.canonicalize().map_err(|_| {
        anyhow!(
            "File not found: '{}'. Please check the path and try again.",
            file_path
        )
    })?;

    let canonical_workspace = workspace_root.canonicalize().unwrap_or_else(|_| workspace_root.to_path_buf());

    if !canonical.starts_with(&canonical_workspace) {
        return Err(anyhow!(
            "Access denied: '{}' is outside the workspace root '{}'. File operations are restricted to the project directory.",
            file_path,
            workspace_root.display()
        ));
    }

    if !canonical.is_file() {
        return Err(anyhow!(
            "Path exists but is not a file: '{}'. Expected a regular file.",
            file_path
        ));
    }
    Ok(())
}

/// Validate that an ID parameter contains only safe characters
pub(crate) fn validate_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(anyhow!("{} cannot be empty", field));
    }
    if !value.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_') {
        return Err(anyhow!("{} contains invalid characters: {}", field, value));
    }
    Ok(())
}

/// Parse a positive integer parameter with optional range validation
fn parse_positive_int(
    args: &Map<String, Value>,
    field: &str,
    min: Option<u64>,
    max: Option<u64>,
) -> Result<Option<u64>> {
    match args.get(field) {
        Some(Value::Number(n)) => {
            let val = n.as_u64().ok_or_else(|| {
                anyhow!(
                    "{} must be a positive integer (got: {})",
                    field,
                    n
                )
            })?;

            // Validate minimum
            if let Some(min_val) = min {
                if val < min_val {
                    return Err(anyhow!(
                        "{} must be at least {} (got: {})",
                        field,
                        min_val,
                        val
                    ));
                }
            }

            // Validate maximum
            if let Some(max_val) = max {
                if val > max_val {
                    return Err(anyhow!(
                        "{} must be at most {} (got: {})",
                        field,
                        max_val,
                        val
                    ));
                }
            }

            Ok(Some(val))
        }
        Some(val) => Err(anyhow!(
            "{} must be a number, but got: {}",
            field,
            get_value_type_name(val)
        )),
        None => Ok(None),
    }
}

/// Parse an enum parameter from arguments with validation
fn parse_enum_param<T>(args: &Map<String, Value>, field: &str) -> Result<Option<T>>
where
    T: for<'de> serde::Deserialize<'de> + fmt::Display + EnumValues,
{
    match args.get(field) {
        Some(Value::String(s)) => {
            // Parse using serde's string deserializer
            let value: T = serde_json::from_value(Value::String(s.clone())).map_err(|_| {
                let valid = T::valid_values().join(", ");
                anyhow!(
                    "Invalid value for {}: '{}'. Valid values are: [{}]",
                    field,
                    s,
                    valid
                )
            })?;
            Ok(Some(value))
        }
        Some(Value::Null) => Ok(None),  // Treat null as absent
        Some(val) => Err(anyhow!("{} must be a string, but got: {}", field, get_value_type_name(val))),
        None => Ok(None),
    }
}

// ========== Generic API Handlers ==========

/// Generic POST handler - creates or updates a resource
fn api_post(
    server: &Server,
    endpoint: &str,
    args: Map<String, Value>,
) -> Result<Value> {
    server.api.post(endpoint, Value::Object(args))
}

/// Generic PUT handler - updates a resource
fn api_put(
    server: &Server,
    endpoint: &str,
    args: Map<String, Value>,
) -> Result<Value> {
    server.api.put(endpoint, Value::Object(args))
}

/// Generic GET by ID handler
fn api_get_by_id(
    server: &Server,
    endpoint_template: &str,  // e.g., "/api/tasks/{}"
    id_field: &str,            // e.g., "task_id"
    args: &Map<String, Value>,
) -> Result<Value> {
    let id = args
        .get(id_field)
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: {}", id_field))?;
    validate_id(id_field, id)?;

    server.api.get(&endpoint_template.replace("{}", id), None)
}

/// Generic GET with query filters
fn api_get_with_filters(
    server: &Server,
    endpoint: &str,
    required_fields: &[&str],
    optional_fields: &[&str],
    args: &Map<String, Value>,
) -> Result<Value> {
    let mut query = Vec::new();

    // Add required query parameters
    for &field in required_fields {
        let value = args
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing required parameter: {}", field))?;
        query.push((field.to_string(), value.to_string()));
    }

    // Add optional parameters (string, boolean, or integer)
    for &field in optional_fields {
        if let Some(value) = args.get(field).and_then(|v| v.as_str()) {
            query.push((field.to_string(), value.to_string()));
        } else if let Some(value) = args.get(field).and_then(|v| v.as_bool()) {
            query.push((field.to_string(), value.to_string()));
        } else if let Some(value) = args.get(field).and_then(|v| v.as_u64()) {
            query.push((field.to_string(), value.to_string()));
        } else if let Some(arr) = args.get(field).and_then(|v| v.as_array()) {
            let joined: Vec<String> = arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect();
            if !joined.is_empty() {
                query.push((field.to_string(), joined.join(",")));
            }
        }
    }

    server.api.get(endpoint, Some(query))
}

/// Generic DELETE handler
fn api_delete(
    server: &Server,
    endpoint_template: &str,  // e.g., "/api/tasks/{}/dependencies/{}"
    id_fields: &[&str],        // e.g., ["task_id", "depends_on"]
    args: &Map<String, Value>,
) -> Result<Value> {
    let mut endpoint = endpoint_template.to_string();

    for &field in id_fields {
        let value = args
            .get(field)
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("Missing required parameter: {}", field))?;
        validate_id(field, value)?;

        // Replace first occurrence of {}
        endpoint = endpoint.replacen("{}", value, 1);
    }

    server.api.delete(&endpoint)
}

/// Route tool calls to implementations
pub fn call_tool(
    server: &mut Server,
    name: &str,
    mut args: Map<String, Value>,
) -> Result<Value> {
    // Auto-fill project_id from CLAUDE.md default if not provided
    if args.get("project_id").map_or(true, |v| v.as_str().map_or(true, |s| s.is_empty())) {
        if let Some(ref default_pid) = server.default_project_id {
            // Only inject if the tool's schema actually uses project_id
            let tools_with_project_id = [
                "load_session_state", "save_session_state", "get_sessions",
                "get_session_code_context", "create_project",
                "get_tasks", "search_tasks", "save_task",
                "get_documents", "save_document", "update_document", "get_plan",
                "save_plan", "get_plans", "save_decision", "get_decisions",
                "get_project_estimate_accuracy",
                "add_graph_edge", "query_graph", "get_graph_neighbors",
                "query_context", "search_code_context", "unified_search",
                "recent_activity",
            ];
            if tools_with_project_id.contains(&name) {
                args.insert("project_id".to_string(), Value::String(default_pid.clone()));
            }
        }
    }

    match name {
        "load_session_state" => load_session_state(server, args),
        "save_session_state" => save_session_state(server, args),
        "get_sessions" => get_sessions(server, args),
        "get_session" => get_session(server, args),
        "get_session_code_context" => get_session_code_context(server, args),
        "create_project" => create_project(server, args),
        "get_tasks" => get_tasks(server, args),
        "search_tasks" => search_tasks(server, args),
        "get_task" => get_task(server, args),
        "get_task_chain" => get_task_chain(server, args),
        "get_execution_waves" => get_execution_waves(server, args),
        "save_task" => save_task(server, args),
        "update_task_status" => update_task_status(server, args),
        "update_task" => update_task(server, args),
        "task_dependency" => task_dependency(server, args),
        "add_progress_log" => add_progress_log(server, args),
        "save_document" => save_document(server, args),
        "get_documents" => get_documents(server, args),
        "get_document" => get_document(server, args),
        "update_document" => update_document(server, args),
        "link_document" => link_document(server, args),
        "save_plan" => save_plan(server, args),
        "get_plans" => get_plans(server, args),
        "get_plan" => get_plan(server, args),
        "save_decision" => save_decision(server, args),
        "get_decisions" => get_decisions(server, args),
        "get_project_estimate_accuracy" => get_project_estimate_accuracy(server, args),
        // AST Tools
        "parse_file_ast" => parse_file_ast(server, args),
        "analyze_file" => analyze_file_tool(args, &server.workspace_root),
        "analyze_symbol" => analyze_symbol_tool(args, &server.workspace_root),
        "analyze_files" => analyze_files_tool(args, &server.workspace_root),
        "analyze_changes" => analyze_changes_tool(args, &server.workspace_root),
        "diff_file_ast" => diff_file_ast_tool(args, &server.workspace_root),
        "find_dead_code" => find_dead_code_tool(args, server),
        "find_references" => find_references_tool(args, server),
        "find_interface_implementations" => find_interface_implementations_tool(args, &server.workspace_root),
        "contextual_search" => contextual_search_tool(args, &server.workspace_root),
        "project_info" => project_info_tool(args, server),
        "get_project_map" => scan_project_map_tool(args, server),
        // CSS Analysis Tools
        "find_css_rules" => find_css_rules_tool(args, &server.workspace_root),
        "find_unused_selectors" => find_unused_selectors_tool(args, &server.workspace_root),
        // CSS Analysis Tools V2
        "resolve_tailwind_classes" => resolve_tailwind_classes_tool(args, &server.workspace_root),
        // LSP Tools
        "lsp" => lsp_query(args, &server.lsp),
        "lsp_workspace_symbols" => lsp_workspace_symbols(args, &server.lsp),
        "lsp_document_symbols" => lsp_document_symbols(args, &server.lsp),
        // Engine V2 Intelligence Tools
        "search_code_context" => search_code_context(server, args),
        "read_symbol_lines" => read_symbol_lines(args, &server.workspace_root),
        "init_vector_db" => init_vector_db(server, args),
        "query_context" => query_context(server, args),
        "add_graph_edge" => add_graph_edge(server, args),
        "query_graph" => query_graph(server, args),
        "get_graph_neighbors" => get_graph_neighbors(server, args),
        // Batch execution
        "batch" => batch(server, args),
        // Activity feed
        "recent_activity" => recent_activity(server, args),
        // Knowledge Base
        "kb_ask" => kb_ask(server, args),
        // Unified Search
        "unified_search" => unified_search(server, args),
        // Built-in templates & skills
        "install_skills" => install_skills(args),
        "get_template" => get_template(args),
        _ => Err(anyhow!("Unknown tool: {}", name)),
    }
}

/// Tool implementation: batch - execute multiple tool calls in one request
fn batch(server: &mut Server, args: Map<String, Value>) -> Result<Value> {
    let operations = args
        .get("operations")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("'operations' is required and must be an array"))?;

    if operations.is_empty() {
        return Err(anyhow!("'operations' must contain at least 1 item"));
    }

    if operations.len() > 20 {
        return Err(anyhow!(
            "'operations' must contain at most 20 items, got {}",
            operations.len()
        ));
    }

    let mut results = Vec::with_capacity(operations.len());

    for (index, op) in operations.iter().enumerate() {
        let tool_name = match op.get("tool").and_then(|v| v.as_str()) {
            Some(name) => name,
            None => {
                results.push(json!({
                    "index": index,
                    "tool": null,
                    "success": false,
                    "error": "Each operation must have a 'tool' string field"
                }));
                continue;
            }
        };

        // Block recursive batch calls
        if tool_name == "batch" {
            results.push(json!({
                "index": index,
                "tool": "batch",
                "success": false,
                "error": "Recursive batch calls are not allowed"
            }));
            continue;
        }

        let tool_args = match op.get("args").and_then(|v| v.as_object()) {
            Some(a) => a.clone(),
            None => Map::new(),
        };

        match call_tool(server, tool_name, tool_args) {
            Ok(result) => {
                results.push(json!({
                    "index": index,
                    "tool": tool_name,
                    "success": true,
                    "result": result
                }));
            }
            Err(e) => {
                results.push(json!({
                    "index": index,
                    "tool": tool_name,
                    "success": false,
                    "error": e.to_string()
                }));
            }
        }
    }

    Ok(json!({
        "total": results.len(),
        "results": results
    }))
}

/// Tool implementation: recent_activity — composite view of recent project changes
fn recent_activity(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let hours = args
        .get("hours")
        .and_then(|v| v.as_u64())
        .unwrap_or(24)
        .min(168) as i64;

    // Calculate cutoff timestamp
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let cutoff_secs = now - (hours * 3600);

    // Fetch tasks, documents, decisions, and sessions in parallel isn't possible
    // with synchronous ureq, so we fetch sequentially but it's still fast
    let tasks = server.api.get(
        "/api/v2/tasks",
        Some(vec![("project_id".to_string(), project_id.to_string())]),
    ).unwrap_or(json!([]));

    // CULTRA-938: /api/v2/tasks now returns {tasks, total, limit, offset}
    // (CULTRA-929 A1) instead of a bare []. Unwrap the array before passing
    // to filter_recent. Falls back to the original value if "tasks" isn't
    // present, so this code stays correct against both shapes during the
    // deploy window.
    let tasks = match tasks.get("tasks").cloned() {
        Some(arr) => arr,
        None => tasks,
    };

    let documents = server.api.get(
        "/api/v2/documents",
        Some(vec![("project_id".to_string(), project_id.to_string())]),
    ).unwrap_or(json!([]));

    let decisions = server.api.get(
        "/api/v2/decisions",
        Some(vec![("project_id".to_string(), project_id.to_string())]),
    ).unwrap_or(json!([]));

    let sessions = server.api.get(
        "/api/v2/sessions",
        Some(vec![("project_id".to_string(), project_id.to_string())]),
    ).unwrap_or(json!([]));

    // Filter by updated_at > cutoff
    let filter_recent = |items: &Value| -> Vec<Value> {
        items.as_array().map(|arr| {
            arr.iter().filter(|item| {
                // Try updated_at, then last_active, then created_at
                let ts = item.get("updated_at")
                    .or_else(|| item.get("last_active"))
                    .or_else(|| item.get("created_at"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                parse_timestamp_secs(ts) > cutoff_secs
            }).cloned().collect()
        }).unwrap_or_default()
    };

    let recent_tasks = filter_recent(&tasks);
    let recent_docs = filter_recent(&documents);
    let recent_decisions = filter_recent(&decisions);
    let recent_sessions = filter_recent(&sessions);

    Ok(json!({
        "project_id": project_id,
        "lookback_hours": hours,
        "tasks": {
            "count": recent_tasks.len(),
            "items": recent_tasks.iter().map(|t| json!({
                "task_id": t.get("task_id"),
                "title": t.get("title"),
                "status": t.get("status"),
                "priority": t.get("priority"),
                "updated_at": t.get("updated_at"),
            })).collect::<Vec<_>>()
        },
        "documents": {
            "count": recent_docs.len(),
            "items": recent_docs.iter().map(|d| json!({
                "document_id": d.get("document_id"),
                "title": d.get("title"),
                "doc_type": d.get("doc_type"),
                "updated_at": d.get("updated_at"),
            })).collect::<Vec<_>>()
        },
        "decisions": {
            "count": recent_decisions.len(),
            "items": recent_decisions.iter().map(|d| json!({
                "decision_id": d.get("decision_id"),
                "title": d.get("title"),
                "status": d.get("status"),
                "updated_at": d.get("updated_at"),
            })).collect::<Vec<_>>()
        },
        "sessions": {
            "count": recent_sessions.len(),
            "items": recent_sessions.iter().map(|s| json!({
                "session_id": s.get("session_id"),
                "last_active": s.get("last_active"),
                "working_memory": s.get("working_memory"),
            })).collect::<Vec<_>>()
        }
    }))
}

/// Parse RFC 3339 timestamp to unix seconds. Returns 0 on parse failure.
fn parse_timestamp_secs(ts: &str) -> i64 {
    time::OffsetDateTime::parse(ts, &time::format_description::well_known::Rfc3339)
        .map(|dt| dt.unix_timestamp())
        .unwrap_or(0)
}

/// Tool implementation: kb_ask — query knowledge base with RAG
fn kb_ask(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    server.api.post("/api/v2/kb/ask", serde_json::Value::Object(args))
}

/// Tool implementation: unified_search — search across all entity types
fn unified_search(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let _query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    api_get_with_filters(
        server,
        "/api/v2/search",
        &["project_id", "query"],
        &["entity_types", "limit"],
        &args,
    )
}

/// Tool implementation: install_skills — bootstrap built-in skills into .claude/skills/
fn install_skills(args: Map<String, Value>) -> Result<Value> {
    let force = args.get("force").and_then(|v| v.as_bool()).unwrap_or(false);
    let list_only = args.get("list").and_then(|v| v.as_bool()).unwrap_or(false);

    if list_only {
        let skills: Vec<Value> = BUILT_IN_SKILLS
            .iter()
            .map(|(name, content)| {
                // Extract description from frontmatter
                let desc = content
                    .lines()
                    .find(|l| l.starts_with("description:"))
                    .map(|l| l.strip_prefix("description:").unwrap_or("").trim())
                    .unwrap_or("No description");
                json!({
                    "name": name,
                    "description": desc,
                    "size_bytes": content.len()
                })
            })
            .collect();
        return Ok(json!({
            "available_skills": skills,
            "total": skills.len()
        }));
    }

    // Determine skills directory
    let cwd = std::env::current_dir()
        .map_err(|e| anyhow!("Failed to get current directory: {}", e))?;
    let skills_dir = cwd.join(".claude").join("skills");

    // Create directory if needed
    std::fs::create_dir_all(&skills_dir)
        .map_err(|e| anyhow!("Failed to create .claude/skills/: {}", e))?;

    let mut installed = Vec::new();
    let mut skipped = Vec::new();

    for (name, content) in BUILT_IN_SKILLS {
        // Claude Code expects skills in subdirectories: .claude/skills/<name>/SKILL.md
        let skill_dir = skills_dir.join(name);
        let path = skill_dir.join("SKILL.md");
        if path.exists() && !force {
            skipped.push(json!({
                "name": name,
                "reason": "already exists (use force=true to overwrite)"
            }));
            continue;
        }
        std::fs::create_dir_all(&skill_dir)
            .map_err(|e| anyhow!("Failed to create skill directory {}: {}", name, e))?;
        std::fs::write(&path, content)
            .map_err(|e| anyhow!("Failed to write {}/SKILL.md: {}", name, e))?;
        installed.push(json!({
            "name": name,
            "path": path.to_string_lossy()
        }));
    }

    Ok(json!({
        "installed": installed,
        "skipped": skipped,
        "skills_dir": skills_dir.to_string_lossy()
    }))
}

/// Tool implementation: get_template — return built-in templates embedded at compile time
fn get_template(args: Map<String, Value>) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: name"))?;

    let (title, content) = match name {
        "claude_md" => ("CLAUDE.md Template", CLAUDE_MD_TEMPLATE),
        "template_guide" => ("CLAUDE.md Template Guide", CLAUDE_TEMPLATE_GUIDE),
        other => return Err(anyhow!("Invalid template '{}'. Must be 'claude_md' or 'template_guide'", other)),
    };

    Ok(json!({
        "name": name,
        "title": title,
        "content": content
    }))
}

/// Tool implementation: load_session_state
///
/// CULTRA-1011: after fetching session state, attempts to merge a
/// `project_map` field into the response for boot-up workspace orientation.
/// Scanner failures (including panics) are swallowed — `load_session_state`
/// must NEVER fail boot just because the floorplan couldn't be generated.
fn load_session_state(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate and normalize strategy enum
    if let Some(strategy) = parse_enum_param::<SessionStrategy>(&args, "strategy")? {
        args.insert("strategy".to_string(), Value::String(strategy.to_string()));
    }

    let mut result = api_get_with_filters(
        server,
        "/api/v2/sessions/latest",
        &["project_id"],
        &["strategy", "include_plan_context", "refresh_cache"],
        &args,
    )?;

    merge_project_map_into(&mut result, &server.workspace_root);
    Ok(result)
}

/// Tool implementation: save_session_state
/// CULTRA-941: coerce a field from a JSON-encoded string to an object.
/// Agents sometimes pass nested objects as stringified JSON; this smooths
/// that out before validation / forwarding to the API. Non-string values
/// pass through unchanged.
fn coerce_string_to_object(args: &mut Map<String, Value>, field: &str) -> Result<()> {
    if let Some(Value::String(json_str)) = args.get(field) {
        let parsed: Value = serde_json::from_str(json_str)
            .map_err(|e| anyhow!("Invalid JSON in {}: {}", field, e))?;
        args.insert(field.to_string(), parsed);
    }
    Ok(())
}

/// CULTRA-941: require a non-empty string field on an object. Returns a
/// descriptive error with the full qualified path ("working_memory.phase")
/// and the provided context hint when either the field is missing or
/// present but empty/non-string.
fn require_non_empty_string(
    obj: &Map<String, Value>,
    field: &str,
    namespace: &str,
    hint: &str,
) -> Result<()> {
    match obj.get(field) {
        Some(v) if v.is_string() && !v.as_str().unwrap_or("").trim().is_empty() => Ok(()),
        Some(_) => Err(anyhow!("{}.{} must be a non-empty string ({})", namespace, field, hint)),
        None => Err(anyhow!("{}.{} is required (Engine V3)", namespace, field)),
    }
}

fn save_session_state(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Agents sometimes stringify nested objects — coerce before validating.
    coerce_string_to_object(&mut args, "context_snapshot")?;
    coerce_string_to_object(&mut args, "working_memory")?;

    // Engine V3 validation for working_memory structure.
    if let Some(wm) = args.get("working_memory").and_then(|v| v.as_object()) {
        require_non_empty_string(wm, "phase", "working_memory",
            "e.g., 'Implementation', 'Testing', 'Planning'")?;
        require_non_empty_string(wm, "current_focus", "working_memory",
            "describes what you're doing now")?;
        require_non_empty_string(wm, "next_action", "working_memory",
            "specific next step")?;
    }

    // Engine V3 validation for context_snapshot structure.
    if let Some(cs) = args.get("context_snapshot").and_then(|v| v.as_object()) {
        require_non_empty_string(cs, "next_session_start", "context_snapshot",
            "clear resuming instructions")?;
    }

    api_post(server, "/api/v2/sessions", args)
}

/// Tool implementation: get_sessions
fn get_sessions(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_with_filters(
        server,
        "/api/v2/sessions",
        &["project_id"],
        &["limit", "kind"],
        &args,
    )
}

/// Tool implementation: get_session
fn get_session(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_by_id(server, "/api/v2/sessions/{}", "session_id", &args)
}

/// Tool implementation: get_session_code_context
fn get_session_code_context(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_with_filters(
        server,
        "/api/v2/sessions/code-context",
        &["project_id"],
        &["file_path"],
        &args,
    )
}

/// Tool implementation: create_project
fn create_project(server: &Server, args: Map<String, Value>) -> Result<Value> {
    if !args.contains_key("project_id") || args.get("project_id").and_then(|v| v.as_str()).is_none() {
        return Err(anyhow!("project_id is required"));
    }
    if !args.contains_key("name") || args.get("name").and_then(|v| v.as_str()).is_none() {
        return Err(anyhow!("name is required"));
    }
    api_post(server, "/api/v2/projects", args)
}

/// Tool implementation: get_project_estimate_accuracy (CULTRA-1056/1057).
/// Pure passthrough — the Go endpoint does the aggregation. We validate
/// project_id at the shim layer so the agent gets a fast, clear error
/// rather than a 400 from the API.
fn get_project_estimate_accuracy(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;
    validate_id("project_id", project_id)?;

    server.api.get(&format!("/api/v2/projects/{}/estimate-accuracy", project_id), None)
}

/// Tool implementation: get_tasks
fn get_tasks(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // CULTRA-939: status now accepts either a single string (legacy, normalized
    // via the enum) or an array of strings (validated element-by-element and
    // passed through; api_get_with_filters comma-joins it for the backend).
    match args.get("status").cloned() {
        Some(Value::String(_)) => {
            if let Some(status) = parse_enum_param::<TaskStatus>(&args, "status")? {
                args.insert("status".to_string(), Value::String(status.to_string()));
            }
        }
        Some(Value::Array(arr)) => {
            for v in &arr {
                let s = v.as_str().ok_or_else(|| anyhow!(
                    "status array must contain only strings, got: {}",
                    get_value_type_name(v)
                ))?;
                let _: TaskStatus = serde_json::from_value(Value::String(s.to_string()))
                    .map_err(|_| anyhow!(
                        "Invalid status value '{}'. Valid values: [{}]",
                        s,
                        TaskStatus::valid_values().join(", ")
                    ))?;
            }
        }
        Some(Value::Null) | None => {}
        Some(other) => {
            return Err(anyhow!(
                "status must be a string or array of strings, got: {}",
                get_value_type_name(&other)
            ));
        }
    }

    if let Some(priority) = parse_enum_param::<Priority>(&args, "priority")? {
        args.insert("priority".to_string(), Value::String(priority.to_string()));
    }

    api_get_with_filters(
        server,
        "/api/v2/tasks",
        &["project_id"],
        // CULTRA-939: pagination + sort surfaced for the new /api/v2/tasks
        // wrapped response shape (CULTRA-929 A1).
        &["status", "priority", "assigned_to", "limit", "offset", "sort", "dir"],
        &args,
    )
}

/// Tool implementation: search_tasks
fn search_tasks(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    let mut query_params = vec![
        ("project_id".to_string(), project_id.to_string()),
        ("q".to_string(), query.to_string()),
    ];

    // Add optional limit with validation (1-100)
    if let Some(limit) = parse_positive_int(&args, "limit", Some(1), Some(100))? {
        query_params.push(("limit".to_string(), limit.to_string()));
    }

    server.api.get("/api/v2/tasks/search", Some(query_params))
}

/// Tool implementation: save_task
fn save_task(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate and normalize task enums
    if let Some(task_type) = parse_enum_param::<TaskType>(&args, "type")? {
        args.insert("type".to_string(), Value::String(task_type.to_string()));
    }
    if let Some(status) = parse_enum_param::<TaskStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(priority) = parse_enum_param::<Priority>(&args, "priority")? {
        args.insert("priority".to_string(), Value::String(priority.to_string()));
    }

    api_post(server, "/api/v2/tasks", args)
}

/// Tool implementation: get_task
fn get_task(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_by_id(server, "/api/v2/tasks/{}", "task_id", &args)
}

/// Tool implementation: get_task_chain
fn get_task_chain(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: task_id"))?;
    validate_id("task_id", task_id)?;

    let mut query_params = vec![];

    // Add optional direction parameter
    if let Some(direction) = args.get("direction").and_then(|v| v.as_str()) {
        query_params.push(("direction".to_string(), direction.to_string()));
    }

    // Add optional max_depth parameter with validation (1-10)
    if let Some(max_depth) = parse_positive_int(&args, "max_depth", Some(1), Some(10))? {
        query_params.push(("max_depth".to_string(), max_depth.to_string()));
    }

    let query = if query_params.is_empty() {
        None
    } else {
        Some(query_params)
    };

    server.api.get(&format!("/api/v2/tasks/{}/chain", task_id), query)
}

/// Tool implementation: update_task_status
fn update_task_status(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: task_id"))?
        .to_string(); // Clone the string
    validate_id("task_id", &task_id)?;

    // Validate status enum
    if let Some(status) = parse_enum_param::<TaskStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }

    api_put(server, &format!("/api/v2/tasks/{}", task_id), args)
}

/// Tool implementation: update_task
fn update_task(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: task_id"))?
        .to_string(); // Clone the string
    validate_id("task_id", &task_id)?;

    // Validate and normalize task enums
    if let Some(task_type) = parse_enum_param::<TaskType>(&args, "type")? {
        args.insert("type".to_string(), Value::String(task_type.to_string()));
    }
    if let Some(status) = parse_enum_param::<TaskStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(priority) = parse_enum_param::<Priority>(&args, "priority")? {
        args.insert("priority".to_string(), Value::String(priority.to_string()));
    }

    api_put(server, &format!("/api/v2/tasks/{}", task_id), args)
}

/// Tool implementation: task_dependency (consolidated from add_task_dependency + remove_task_dependency)
fn task_dependency(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let action = args
        .get("action")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: action"))?;

    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: task_id"))?;
    validate_id("task_id", task_id)?;

    let depends_on = args
        .get("depends_on")
        .ok_or_else(|| anyhow!("Missing required parameter: depends_on"))?;
    if let Some(depends_on_str) = depends_on.as_str() {
        validate_id("depends_on", depends_on_str)?;
    }

    match action {
        "add" => {
            let mut api_body = Map::new();
            api_body.insert("blocked_by".to_string(), depends_on.clone());
            api_post(server, &format!("/api/v2/tasks/{}/dependencies", task_id), api_body)
        }
        "remove" => {
            api_delete(
                server,
                "/api/v2/tasks/{}/dependencies/{}",
                &["task_id", "depends_on"],
                &args,
            )
        }
        other => Err(anyhow!("Invalid action '{}'. Must be 'add' or 'remove'", other)),
    }
}

/// Tool implementation: add_progress_log
fn add_progress_log(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let task_id = args
        .get("task_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: task_id"))?;
    validate_id("task_id", task_id)?;

    api_post(server, &format!("/api/v2/tasks/{}/progress", task_id), args)
}

/// Tool implementation: save_document
fn resolve_content_file(args: &mut Map<String, Value>) -> Result<()> {
    if let Some(file_path) = args.remove("content_file").and_then(|v| v.as_str().map(|s| s.to_string())) {
        let file_content = std::fs::read_to_string(&file_path)
            .map_err(|e| anyhow!("Failed to read file '{}': {}", file_path, e))?;
        args.insert("content".to_string(), Value::String(file_content));
    }
    Ok(())
}

fn save_document(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // If content_file is provided, read the file and use as content (overrides content)
    resolve_content_file(&mut args)?;

    // Validate doc_type enum
    if let Some(doc_type) = parse_enum_param::<DocType>(&args, "doc_type")? {
        args.insert("doc_type".to_string(), Value::String(doc_type.to_string()));
    }

    api_post(server, "/api/v2/documents", args)
}

/// Tool implementation: get_documents
fn get_documents(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate doc_type filter enum
    if let Some(doc_type) = parse_enum_param::<DocType>(&args, "doc_type")? {
        args.insert("doc_type".to_string(), Value::String(doc_type.to_string()));
    }

    // Convert tags array to comma-separated string for the API
    if let Some(Value::Array(tags)) = args.remove("tags") {
        let tag_str: Vec<String> = tags.iter()
            .filter_map(|v| v.as_str().map(|s| s.to_string()))
            .collect();
        if !tag_str.is_empty() {
            args.insert("tags".to_string(), Value::String(tag_str.join(",")));
        }
    }

    api_get_with_filters(
        server,
        "/api/v2/documents",
        &["project_id"],
        &["doc_type", "linked_task_id", "tags"],
        &args,
    )
}

/// Tool implementation: get_document
fn get_document(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_by_id(server, "/api/v2/documents/{}", "document_id", &args)
}

/// Tool implementation: update_document
fn update_document(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // If content_file is provided, read the file and use as content (overrides content)
    resolve_content_file(&mut args)?;

    let document_id = args
        .get("document_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: document_id"))?
        .to_string(); // Clone the string
    validate_id("document_id", &document_id)?;

    // Validate doc_type enum
    if let Some(doc_type) = parse_enum_param::<DocType>(&args, "doc_type")? {
        args.insert("doc_type".to_string(), Value::String(doc_type.to_string()));
    }

    api_put(server, &format!("/api/v2/documents/{}", document_id), args)
}

/// Tool implementation: link_document (CULTRA-1065: now accepts task_ids, plan_id, and plan_ids).
fn link_document(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _document_id = args.get("document_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("document_id is required"))?;
    validate_id("document_id", _document_id)?;

    let task_ids = args.get("task_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let plan_ids = args.get("plan_ids")
        .and_then(|v| v.as_array())
        .map(|a| a.as_slice())
        .unwrap_or(&[]);
    let plan_id = args.get("plan_id")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty());

    if task_ids.is_empty() && plan_ids.is_empty() && plan_id.is_none() {
        return Err(anyhow!(
            "at least one of task_ids, plan_id, or plan_ids must be provided"
        ));
    }

    for (i, task_id_val) in task_ids.iter().enumerate() {
        if let Some(task_id) = task_id_val.as_str() {
            validate_id(&format!("task_ids[{}]", i), task_id)?;
        }
    }
    for (i, plan_id_val) in plan_ids.iter().enumerate() {
        if let Some(pid) = plan_id_val.as_str() {
            validate_id(&format!("plan_ids[{}]", i), pid)?;
        }
    }
    if let Some(pid) = plan_id {
        validate_id("plan_id", pid)?;
    }

    api_post(server, "/api/v2/documents/link-batch", args)
}

/// Tool implementation: save_plan
fn save_plan(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate plan status enum
    if let Some(status) = parse_enum_param::<PlanStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(priority) = parse_enum_param::<Priority>(&args, "priority")? {
        args.insert("priority".to_string(), Value::String(priority.to_string()));
    }

    // Engine V3 validation for structured plan fields
    if let Some(problem) = args.get("problem") {
        if let Some(problem_str) = problem.as_str() {
            if problem_str.trim().is_empty() {
                return Err(anyhow!("problem cannot be empty (Engine V3 requirement)"));
            }
        }
    }

    if let Some(goal) = args.get("goal") {
        if let Some(goal_str) = goal.as_str() {
            if goal_str.trim().is_empty() {
                return Err(anyhow!("goal cannot be empty (Engine V3 requirement)"));
            }
        }
    }

    // Validate flexible content fields
    if let Some(content) = args.get("content") {
        if let Some(content_obj) = content.as_object() {
            // Validate success_criteria if present
            if let Some(success_criteria) = content_obj.get("success_criteria") {
                if let Some(criteria_array) = success_criteria.as_array() {
                    for (i, item) in criteria_array.iter().enumerate() {
                        if !item.is_string() {
                            return Err(anyhow!(
                                "content.success_criteria[{}] must be a string (got: {:?})",
                                i,
                                item
                            ));
                        }
                    }
                } else if !success_criteria.is_null() {
                    return Err(anyhow!(
                        "content.success_criteria must be an array of strings (got: {:?})",
                        success_criteria
                    ));
                }
            }
        }
    }

    api_post(server, "/api/v2/plans", args)
}

/// Tool implementation: get_plans
fn get_plans(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate plan status filter enum
    if let Some(status) = parse_enum_param::<PlanStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }

    api_get_with_filters(
        server,
        "/api/v2/plans",
        &["project_id"],
        &["status"],
        &args,
    )
}

/// Tool implementation: get_plan (consolidated from get_plan_status + get_plan_details).
/// CULTRA-1049/1050 added detail="ascii" → /plans/:id/ascii with optional
/// width / style / with_titles render-knob passthrough.
fn get_plan(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let plan_id = args
        .get("plan_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: plan_id"))?;
    validate_id("plan_id", plan_id)?;

    let detail = args.get("detail").and_then(|v| v.as_str()).unwrap_or("status");
    let endpoint = match detail {
        "status" => format!("/api/v2/plans/{}/status", plan_id),
        "full" => format!("/api/v2/plans/{}/details", plan_id),
        "ascii" => format!("/api/v2/plans/{}/ascii", plan_id),
        other => return Err(anyhow!(
            "Invalid detail value '{}'. Must be 'status', 'full', or 'ascii'", other
        )),
    };

    // Render knobs are only meaningful for detail='ascii'. We could silently
    // drop them when detail!='ascii', but failing loudly catches a broader
    // class of mistakes (e.g. agent typed detail='status' when it meant
    // 'ascii' but supplied with_titles=true). Cheap insurance.
    let render_query = build_plan_render_query(&args, detail)?;
    let query = if render_query.is_empty() { None } else { Some(render_query) };
    server.api.get(&endpoint, query)
}

/// Validate and collect query-string params for the /plans/:id/* endpoints.
/// Two clusters of params are gated by `detail`:
///
///   - Render knobs (width/style/with_titles): only valid when detail='ascii'
///   - group_by: only valid when detail='status' (CULTRA-1062)
///
/// The function name dates to when it only handled render knobs; left as-is
/// to keep the existing test surface stable.
///
/// Returns an empty vec when the caller supplied no extra params. Mirrors
/// the validation used by execution_waves.rs::build_waves_query_params so
/// the two entry points reject the same shape of bad inputs.
fn build_plan_render_query(args: &Map<String, Value>, detail: &str) -> Result<Vec<(String, String)>> {
    let has_render_knob = args.contains_key("width")
        || args.contains_key("style")
        || args.contains_key("with_titles")
        || args.contains_key("compact_parallel");

    if has_render_knob && detail != "ascii" {
        return Err(anyhow!(
            "width / style / with_titles / compact_parallel only apply when detail='ascii' (got detail='{}')",
            detail
        ));
    }

    let has_group_by = args.contains_key("group_by");
    if has_group_by && detail != "status" {
        return Err(anyhow!(
            "group_by only applies when detail='status' (got detail='{}')",
            detail
        ));
    }

    let mut out: Vec<(String, String)> = vec![];

    if let Some(w) = args.get("width") {
        let n = w
            .as_u64()
            .ok_or_else(|| anyhow!("width must be a positive integer"))?;
        if n == 0 {
            return Err(anyhow!("width must be > 0"));
        }
        out.push(("width".to_string(), n.to_string()));
    }

    if let Some(s) = args.get("style").and_then(|v| v.as_str()) {
        if s != "unicode" && s != "ascii" {
            return Err(anyhow!(
                "invalid style '{}'. Must be 'unicode' or 'ascii'", s
            ));
        }
        out.push(("style".to_string(), s.to_string()));
    }

    if let Some(flag) = args.get("with_titles").and_then(|v| v.as_bool()) {
        if flag {
            out.push(("with_titles".to_string(), "true".to_string()));
        }
    }

    // CULTRA-1059: with_handles=false drops T-handles. Default (absent or
    // true) preserves historical output, so we only emit the param when
    // explicitly false.
    if let Some(flag) = args.get("with_handles").and_then(|v| v.as_bool()) {
        if !flag {
            out.push(("with_handles".to_string(), "false".to_string()));
        }
    }

    // CULTRA-1069: compact_parallel=true collapses N independent components
    // into a single combined wave stanza. Default off preserves historical
    // partitioned form. Only emit when explicitly true.
    if let Some(flag) = args.get("compact_parallel").and_then(|v| v.as_bool()) {
        if flag {
            out.push(("compact_parallel".to_string(), "true".to_string()));
        }
    }

    if let Some(g) = args.get("group_by").and_then(|v| v.as_str()) {
        if g != "tag" {
            return Err(anyhow!(
                "invalid group_by '{}'. Must be 'tag' (only supported value for v1)", g
            ));
        }
        out.push(("group_by".to_string(), g.to_string()));
    }

    Ok(out)
}

/// Tool implementation: save_decision
fn save_decision(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate decision status enum
    if let Some(status) = parse_enum_param::<DecisionStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }

    api_post(server, "/api/v2/decisions", args)
}

/// Tool implementation: get_decisions
fn get_decisions(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate decision status filter enum
    if let Some(status) = parse_enum_param::<DecisionStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }

    api_get_with_filters(
        server,
        "/api/v2/decisions",
        &["project_id"],
        &["status"],
        &args,
    )
}

// ========== AST Tool Implementations ==========

/// Tool implementation: parse_file_ast
fn parse_file_ast(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?
        .to_string();

    // Validate file exists, is readable, and is within workspace
    validate_file_exists(&file_path, &server.workspace_root)?;

    // CULTRA-959: project_id resolution. Was unwrap_or("proj-cultra") which
    // dropped the AST under Cultra's own meta-project regardless of the
    // user's actual workspace — silently breaking subsequent
    // search_code_context calls. The dead-code resolve_project_id helper
    // (already in this file) was written exactly for this case but never
    // wired in. It (a) prefers an explicit args["project_id"] when set,
    // (b) falls back to the harness-injected default_project_id when
    // available, (c) errors with a clear message when neither is set.
    resolve_project_id(server, &mut args)?;
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("project_id resolution succeeded but value is missing"))?
        .to_string();

    // CULTRA-906: optional cross-file caller enrichment via LSP references.
    let with_callers = args
        .get("with_callers")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-994: optional body preview — include the first N lines of each
    // symbol's source so callers can orient without a separate Read call.
    let preview_lines = args
        .get("preview_lines")
        .and_then(|v| v.as_u64())
        .map(|v| v as usize);

    // Parse file locally (fast, no network overhead)
    let parser = Parser::new();
    let file_context = parser.parse_file(&file_path)
        .map_err(|e| anyhow!("Failed to parse file: {}", e))?;

    // POST to API for storage (respects RLS, enables search_code_context)
    let body = json!({
        "project_id": project_id,
        "file_path": file_context.file_path,
        "language": file_context.language,
        "symbols": file_context.symbols,
        "imports": file_context.imports,
        "ast_stats": file_context.ast_stats
    });

    // unwrap: body is constructed above via the json!({...}) literal, which
    // always produces Value::Object — as_object() is infallible here.
    api_post(server, "/api/v2/ast/parse", body.as_object().unwrap().clone())?;

    // Build symbols array, optionally enriched with caller lookups.
    // Enrichment is best-effort: any LSP failure degrades gracefully to
    // returning the symbol without a callers field.
    let symbols_value = if with_callers {
        enrich_symbols_with_callers(&file_context, server)
    } else {
        serde_json::to_value(&file_context.symbols)
            .map_err(|e| anyhow!("Failed to serialize symbols: {}", e))?
    };

    // CULTRA-994: enrich symbols with body preview if requested.
    let symbols_value = if let Some(n) = preview_lines {
        let source = std::fs::read_to_string(&file_context.file_path).unwrap_or_default();
        let source_lines: Vec<&str> = source.lines().collect();
        match symbols_value {
            Value::Array(syms) => {
                let enriched: Vec<Value> = syms.into_iter().map(|mut sym| {
                    if let Value::Object(ref mut obj) = sym {
                        let start = obj.get("line")
                            .and_then(|v| v.as_u64())
                            .unwrap_or(1) as usize;
                        let from = start.saturating_sub(1);
                        let to = (from + n).min(source_lines.len());
                        let preview: Vec<String> = source_lines[from..to]
                            .iter().map(|s| s.to_string()).collect();
                        obj.insert("preview".to_string(), json!(preview));
                    }
                    sym
                }).collect();
                Value::Array(enriched)
            }
            other => other,
        }
    } else {
        symbols_value
    };

    // Return AST metadata
    Ok(json!({
        "success": true,
        "file_path": file_context.file_path,
        "language": file_context.language,
        "symbols": symbols_value,
        "imports": file_context.imports,
        "ast_stats": file_context.ast_stats
    }))
}

/// CULTRA-909: diff_file_ast — structural diff between two git revisions
/// of the same file. Match symbols by name. Same name in both → potentially
/// modified (signature delta). Name only in base → removed. Name only in
/// head → added. No fuzzy rename detection (a renamed function shows as
/// removed+added; the agent can correlate by signature similarity).
fn diff_file_ast_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    use crate::ast::parser::Parser;
    use crate::workspace::git_repo_root;
    use std::collections::HashMap;

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;
    let base_ref = args
        .get("base_ref")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: base_ref"))?;
    let head_ref = args
        .get("head_ref")
        .and_then(|v| v.as_str())
        .unwrap_or("HEAD");

    validate_file_exists(file_path, workspace_root)?;
    let abs_path = std::path::Path::new(file_path);

    // CULTRA-954: walk up from file_path to find the actual git repo root
    // (the dir containing `.git`). The previous implementation assumed
    // `workspace_root == git repo root`, which is only true when the MCP
    // sandbox is launched at the repo root. Any nested layout — multi-crate
    // workspace, monorepo, repo-in-subdirectory — broke with `git show
    // HEAD:wrong/path`. Same family as CULTRA-952.
    let repo = git_repo_root(abs_path, workspace_root).ok_or_else(|| {
        anyhow!(
            "No .git directory found between '{}' and the sandbox root '{}'. \
             diff_file_ast requires the file to live inside a git repository.",
            file_path,
            workspace_root.display()
        )
    })?;

    // Compute file_path relative to the git repo root, not the sandbox.
    // canonical_file is needed because repo.root is canonicalized — both
    // sides of strip_prefix must match symlinks consistently.
    let canonical_file = abs_path
        .canonicalize()
        .map_err(|e| anyhow!("Failed to canonicalize '{}': {}", file_path, e))?;
    let rel_path = repo
        .relative_path(&canonical_file)
        .map_err(|e| anyhow!("file_path is not inside the resolved git repo: {}", e))?
        .to_string_lossy()
        .into_owned();
    let git_root = repo.root.clone();

    // Pull the file at each ref into a tempfile, parse, return symbols.
    // Run git from the resolved repo root so it doesn't depend on the
    // process cwd (which may be the sandbox root, not the repo root).
    let parser = Parser::new();
    let parse_at_ref = |gitref: &str| -> Result<Vec<crate::ast::types::Symbol>> {
        let output = std::process::Command::new("git")
            .args(["show", &format!("{}:{}", gitref, rel_path)])
            .current_dir(&git_root)
            .output()
            .map_err(|e| anyhow!("git show failed to spawn: {}", e))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
            return Err(anyhow!("git show {}:{} failed: {}", gitref, rel_path, stderr.trim()));
        }
        // Write to a temp file preserving the original extension so the
        // parser dispatches to the right language frontend. Use stdlib
        // temp_dir + a unique filename based on PID + nanos to avoid
        // pulling tempfile into runtime deps.
        let suffix = abs_path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| format!(".{}", e))
            .unwrap_or_default();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let tmp_path = std::env::temp_dir().join(format!(
            "ast-diff-{}-{}{}",
            std::process::id(),
            nanos,
            suffix
        ));
        std::fs::write(&tmp_path, &output.stdout)
            .map_err(|e| anyhow!("temp write failed: {}", e))?;
        let path_str = tmp_path.to_string_lossy().into_owned();
        let parse_result = parser.parse_file(&path_str);
        // Best-effort cleanup; we don't fail the call on cleanup errors.
        let _ = std::fs::remove_file(&tmp_path);
        let ctx = parse_result.map_err(|e| anyhow!("parse {}: {}", gitref, e))?;
        Ok(ctx.symbols)
    };

    let base_symbols = parse_at_ref(base_ref)?;
    let head_symbols = parse_at_ref(head_ref)?;

    // Index by symbol name. Multiple symbols with the same name (overloads,
    // method receivers) are collapsed by collecting into Vec.
    let mut base_index: HashMap<String, Vec<&crate::ast::types::Symbol>> = HashMap::new();
    for s in &base_symbols {
        base_index.entry(s.name.clone()).or_default().push(s);
    }
    let mut head_index: HashMap<String, Vec<&crate::ast::types::Symbol>> = HashMap::new();
    for s in &head_symbols {
        head_index.entry(s.name.clone()).or_default().push(s);
    }

    let mut added: Vec<Value> = Vec::new();
    let mut removed: Vec<Value> = Vec::new();
    let mut modified: Vec<Value> = Vec::new();

    // The Go parser stores signature as "name(...)" without param details, so
    // we build a richer comparison key by joining the parameter list + return
    // type. Two symbols are "the same" iff this composite key matches.
    let comparison_key = |s: &crate::ast::types::Symbol| -> String {
        let params: Vec<String> = s.parameters.iter()
            .map(|p| format!("{}:{}", p.name, p.param_type))
            .collect();
        format!(
            "{}|{}|{}",
            s.signature,
            params.join(","),
            s.return_type.clone().unwrap_or_default()
        )
    };

    let symbol_summary = |s: &crate::ast::types::Symbol| -> Value {
        let params: Vec<Value> = s.parameters.iter()
            .map(|p| json!({"name": p.name, "type": p.param_type}))
            .collect();
        json!({
            "name": s.name,
            "type": format!("{:?}", s.symbol_type),
            "line": s.line,
            "signature": s.signature,
            "parameters": params,
            "return_type": s.return_type,
        })
    };

    // Symbols only in base → removed.
    for (name, syms) in &base_index {
        if !head_index.contains_key(name) {
            for s in syms {
                removed.push(symbol_summary(s));
            }
        }
    }
    // Symbols only in head → added.
    for (name, syms) in &head_index {
        if !base_index.contains_key(name) {
            for s in syms {
                added.push(symbol_summary(s));
            }
        }
    }
    // Symbols in both → modified iff comparison key (signature + params + return) changed.
    for (name, base_syms) in &base_index {
        if let Some(head_syms) = head_index.get(name) {
            for (b, h) in base_syms.iter().zip(head_syms.iter()) {
                if comparison_key(b) != comparison_key(h) {
                    modified.push(json!({
                        "name": name,
                        "type": format!("{:?}", b.symbol_type),
                        "base": symbol_summary(b),
                        "head": symbol_summary(h),
                    }));
                }
            }
        }
    }

    Ok(json!({
        "file_path": file_path,
        "base_ref": base_ref,
        "head_ref": head_ref,
        "added": added,
        "removed": removed,
        "modified": modified,
        "summary": {
            "added": added.len(),
            "removed": removed.len(),
            "modified": modified.len(),
            "base_symbol_count": base_symbols.len(),
            "head_symbol_count": head_symbols.len(),
        },
    }))
}

/// CULTRA-908: analyze_changes — wrap analyze_files with a git-diff file
/// list. Limits the analyzer scope to files changed between a ref and HEAD,
/// filtered by language for the chosen analyzer.
fn analyze_changes_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let since = args
        .get("since")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: since"))?;

    let analyzer = args
        .get("analyzer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: analyzer"))?;
    validate_analyzer(analyzer)?;

    // CULTRA-956: include working-tree changes by default. The pre-fix
    // version ran `git diff <since> HEAD`, which only considers committed
    // changes between two refs and excludes uncommitted work. The natural
    // use case for this tool is "analyze what my branch introduced," which
    // almost always means uncommitted work, so the empty-result was a trap.
    let include_working_tree = args
        .get("include_working_tree")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    // git diff --name-only --diff-filter=AM excludes deleted files (D) but
    // keeps added (A) and modified (M). When include_working_tree is true
    // we omit the HEAD endpoint so `git diff <since>` compares the working
    // tree against `<since>` (which is what the user actually wants).
    let mut git_args: Vec<&str> = vec!["diff", "--name-only", "--diff-filter=AM", since];
    if !include_working_tree {
        git_args.push("HEAD");
    }
    let output = std::process::Command::new("git")
        .args(&git_args)
        .current_dir(workspace_root)
        .output()
        .map_err(|e| anyhow!("failed to invoke git: {}", e))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
        return Err(anyhow!("git diff failed: {}", stderr.trim()));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let extensions = analyzer_extensions(analyzer);
    let mut file_paths: Vec<Value> = Vec::new();
    let mut total_changed: usize = 0;
    for relpath in stdout.lines() {
        if relpath.is_empty() {
            continue;
        }
        total_changed += 1;
        // Filter by extension for the chosen analyzer.
        if !file_matches_analyzer(relpath, &extensions) {
            continue;
        }
        let abs = workspace_root.join(relpath);
        // Skip files that no longer exist on disk (e.g., a quick deletion
        // race or an unborn submodule path).
        if !abs.exists() {
            continue;
        }
        file_paths.push(json!(abs.to_string_lossy()));
    }

    if file_paths.is_empty() {
        // CULTRA-956: distinguish "no diff at all" from "language filter
        // rejected files." Pre-fix wording said the latter even when the
        // real cause was the former, which sent users debugging the wrong
        // problem.
        let message = if total_changed == 0 {
            if include_working_tree {
                format!(
                    "No changes found between '{}' and the working tree. \
                     If you expected uncommitted changes here, double-check that 'since' \
                     points at the right ref. To restrict to committed-only diffs, \
                     pass include_working_tree=false.",
                    since
                )
            } else {
                format!(
                    "No committed changes found between '{}' and HEAD. \
                     To include uncommitted working-tree changes, pass include_working_tree=true \
                     (the new default after CULTRA-956).",
                    since
                )
            }
        } else {
            format!(
                "Found {} changed file(s) but none matched the '{}' analyzer's language filter (extensions: {:?})",
                total_changed, analyzer, extensions
            )
        };
        return Ok(json!({
            "analyzer": analyzer,
            "since": since,
            "include_working_tree": include_working_tree,
            "total": 0,
            "total_changed_files": total_changed,
            "succeeded": 0,
            "failed": 0,
            "results": [],
            "message": message,
        }));
    }

    // Delegate to analyze_files. Build a fresh args map; analyze_files_tool
    // does its own validation. CULTRA-1066: forward complexity filters so
    // a git-scoped complexity sweep can also be filtered down.
    let mut delegate_args = Map::new();
    delegate_args.insert("analyzer".to_string(), json!(analyzer));
    delegate_args.insert("file_paths".to_string(), Value::Array(file_paths));
    for key in &["min_cyclomatic", "min_cognitive", "top_n"] {
        if let Some(v) = args.get(*key) {
            delegate_args.insert((*key).to_string(), v.clone());
        }
    }
    let mut result = analyze_files_tool(delegate_args, workspace_root)?;

    // Annotate with the git ref used so the response is self-describing.
    if let Value::Object(ref mut obj) = result {
        obj.insert("since".to_string(), json!(since));
        obj.insert("include_working_tree".to_string(), json!(include_working_tree));
    }
    Ok(result)
}

/// Map an analyzer name to the file extensions it supports. Used to filter
/// the git-diff output before delegating to analyze_files.
fn analyzer_extensions(analyzer: &str) -> &'static [&'static str] {
    match analyzer {
        "concurrency" => &["go"],
        "react" => &["tsx", "jsx", "ts", "js"],
        "css" => &["css", "scss", "sass"],
        "css_variables" => &["css", "scss", "sass"],
        "security" => &["go", "py", "js", "jsx", "ts", "tsx", "rs", "tf"],
        "complexity" => &["go", "py", "js", "jsx", "ts", "tsx", "rs", "tf"],
        _ => &[],
    }
}

/// True if `path` ends with one of the listed extensions (case-insensitive).
fn file_matches_analyzer(path: &str, extensions: &[&str]) -> bool {
    let lower = path.to_lowercase();
    extensions.iter().any(|ext| lower.ends_with(&format!(".{}", ext)))
}

/// CULTRA-907: find_dead_code — flag exported callables in a file that have
/// no references (or only the declaration site itself) anywhere in the workspace.
/// Requires a running language server. Documents per-symbol caveats.
fn find_dead_code_tool(args: Map<String, Value>, server: &Server) -> Result<Value> {
    use crate::ast::parser::Parser;
    use crate::mcp::types::{Scope, SymbolType};

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;
    validate_file_exists(file_path, &server.workspace_root)?;

    // CULTRA-947: strict-mode opt-in. When true, a cold LSP index returns
    // an early error rather than a silently-empty result set.
    let require_warm_index = args
        .get("require_warm_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-950: optional active warmup. When true, runs the per-language
    // warmup command (cargo check / go build / tsc --noEmit) before
    // querying LSP, so the index is populated. Cached per session.
    let do_warmup = args
        .get("warmup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let parser = Parser::new();
    let file_context = parser.parse_file(file_path)
        .map_err(|e| anyhow!("Failed to parse file: {}", e))?;

    // Warmup runs after we know the language but before any LSP calls.
    let warmup_report: Option<crate::lsp::manager::WarmupReport> = if do_warmup {
        Some(server.lsp.ensure_warm(&file_context.language.to_string(), std::path::Path::new(&file_context.file_path)))
    } else {
        None
    };

    let file_contents = std::fs::read_to_string(&file_context.file_path)
        .map_err(|e| anyhow!("Failed to read file: {}", e))?;
    let lines: Vec<&str> = file_contents.lines().collect();

    let is_exported = |s: &Scope| matches!(s, Scope::Public | Scope::Exported);
    let is_callable = |t: &SymbolType| matches!(t, SymbolType::Function | SymbolType::Method);

    let mut dead: Vec<Value> = Vec::new();
    let mut checked: usize = 0;
    let mut lsp_failures: usize = 0;
    // CULTRA-947: track whether ANY symbol has external (non-declaration)
    // references. A warm LSP index reports at least the declaration site for
    // every symbol, so "total_refs > 0" isn't a useful warmth signal — it'd
    // fire for cold indexes on gopls too. The useful signal is whether LSP
    // found any CROSS-site callers. If every symbol's external_refs comes
    // back as zero, the file is either genuinely all-dead or the index hasn't
    // picked up the callers yet — and a cold index is by far the more common
    // explanation in practice.
    let mut any_symbol_has_external_refs = false;

    for sym in &file_context.symbols {
        if !is_callable(&sym.symbol_type) || !is_exported(&sym.scope) {
            continue;
        }
        checked += 1;

        // Locate symbol-name column on its declared line.
        let line_idx = sym.line.saturating_sub(1) as usize;
        let column = match lines.get(line_idx).and_then(|l| l.find(sym.name.as_str())) {
            Some(c) => c as u32,
            None => continue, // Can't locate name → skip rather than false-positive.
        };
        let lsp_line = sym.line.saturating_sub(1);

        let mut lsp_args = Map::new();
        lsp_args.insert("action".to_string(), json!("references"));
        lsp_args.insert("file_path".to_string(), json!(file_context.file_path));
        lsp_args.insert("line".to_string(), json!(lsp_line));
        lsp_args.insert("character".to_string(), json!(column));
        // CULTRA-963: forward warmup so the retry loop handles slow LS indexing.
        // Only the first symbol pays the warmup cost; subsequent are cached.
        if do_warmup {
            lsp_args.insert("warmup".to_string(), json!(true));
        }

        let refs_value = match crate::lsp::tools::lsp_query(lsp_args, &server.lsp) {
            Ok(v) => v,
            Err(_) => {
                lsp_failures += 1;
                continue; // LSP failure → skip; do NOT flag as dead.
            }
        };

        let refs = refs_value.get("references")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let total_refs = refs.len();

        // A symbol is "dead" if every reference is either the declaration
        // site itself or absent. include_declaration=true means the
        // declaration is normally included; we filter it out before counting.
        let external_refs = refs.iter().filter(|loc| {
            let same_file = loc
                .get("uri")
                .and_then(|u| u.as_str())
                .map(|u| u.ends_with(&file_context.file_path))
                .unwrap_or(false);
            let same_line = loc
                .get("range")
                .and_then(|r| r.get("start"))
                .and_then(|s| s.get("line"))
                .and_then(|l| l.as_u64())
                .map(|l| (l as u32) == lsp_line)
                .unwrap_or(false);
            !(same_file && same_line)
        }).count();
        if external_refs > 0 {
            any_symbol_has_external_refs = true;
        }

        if external_refs == 0 {
            // CULTRA-945: distinguish "proven dead" from "LSP didn't find callers".
            //
            //   high: references list is non-empty AND contains the
            //         declaration site — LSP knows about this symbol and
            //         confirmed no external callers. Safe to delete.
            //
            //   low:  references list is empty. Could be legitimately unused
            //         OR the language server hasn't indexed cross-module
            //         references yet (rust-analyzer is particularly prone to
            //         this on cold start). Verify manually before deleting.
            let confidence = if total_refs > 0 { "high" } else { "low" };
            dead.push(json!({
                "name": sym.name,
                "type": format!("{:?}", sym.symbol_type),
                "line": sym.line,
                "signature": sym.signature,
                "confidence": confidence,
                "total_references": total_refs,
            }));
        }
    }

    // CULTRA-947: classify LSP index status.
    //   warm:    at least one checked symbol returned non-zero references
    //            → LSP is responsive and has some index coverage.
    //   cold:    we checked at least one symbol and NONE had any references
    //            → almost certainly the LSP index is not ready yet, not a
    //            codebase where every exported fn is genuinely unused.
    //   unknown: nothing to check (no exported callables) → can't tell.
    let lsp_index_status = if checked == 0 {
        "unknown"
    } else if any_symbol_has_external_refs {
        "warm"
    } else {
        "cold"
    };

    // Strict-mode opt-in: cold index + require_warm_index → early error so
    // the caller doesn't have to guess whether the empty result is real.
    if lsp_index_status == "cold" && require_warm_index {
        return Err(anyhow!(
            "LSP index is cold: checked {} exported symbol(s) and none returned any references. \
             This almost always means the language server has not yet indexed the workspace. \
             Either wait for indexing to complete and retry, or pass require_warm_index=false \
             to accept best-effort results (with dead_symbols.confidence='low').",
            checked
        ));
    }

    let mut response = json!({
        "file_path": file_context.file_path,
        "language": file_context.language,
        "checked_symbols": checked,
        "dead_symbols": dead,
        "lsp_failures": lsp_failures,
        "lsp_index_status": lsp_index_status,
        "caveats": [
            "Dynamic dispatch (interface methods, reflection) is NOT detected and may produce false positives.",
            "Build-tag-gated callers are NOT visible to LSP unless tags match the indexer config.",
            "Test-only exports (callers in *_test.go) ARE detected by gopls but may not be by all servers.",
            "An LSP failure on a symbol skips it (does not flag as dead). lsp_failures counts these.",
            "Each dead_symbol has a confidence rating (CULTRA-945): 'high' means LSP confirmed the declaration site is the only reference; 'low' means LSP returned an empty references list, which can mean legitimately unused OR LSP has not yet indexed cross-module references (common for rust-analyzer on cold start). Verify 'low' confidence findings before deleting.",
            "lsp_index_status (CULTRA-947): 'warm' means LSP returned references for at least one symbol. 'cold' means every checked symbol returned zero references — almost always an indexing gap, not real dead code. 'unknown' means nothing checkable. Pass require_warm_index=true to fail fast on a cold index.",
            "Warmup race (CULTRA-952 follow-up): a successful warmup_report.status='warm' does NOT guarantee the LSP reference database is queryable on the very next call. cargo check / go build finishes before rust-analyzer / gopls finish populating their cross-file reference indexes — the gap is typically 20-60s. If this response shows lsp_index_status='cold' AND warmup_report.status='warm', the warning field will say so explicitly; retry in ~30s.",
        ],
    });

    // CULTRA-947: when cold, surface a loud top-level warning so the caller
    // sees the issue without having to inspect the caveat list.
    //
    // CULTRA-952 follow-up (Vestige verification): if warmup succeeded but
    // results are still cold, that's the cargo-finished-but-LSP-still-indexing
    // race — callout it explicitly so the agent knows to retry, not give up.
    if lsp_index_status == "cold" {
        let warning_text = build_cold_warning_for_find_dead_code(checked, warmup_report.as_ref());
        if let Value::Object(ref mut obj) = response {
            obj.insert("warning".to_string(), json!(warning_text));
        }
    }

    // CULTRA-950: surface the warmup report so callers see the cost and
    // cache state. Always emitted when warmup was requested, regardless of
    // whether it succeeded — a "failed" warmup is signal too.
    if let Some(report) = warmup_report {
        if let Value::Object(ref mut obj) = response {
            obj.insert("warmup_report".to_string(), serde_json::to_value(&report).unwrap_or(Value::Null));
        }
    }

    Ok(response)
}

/// CULTRA-952 (post-verification): build the cold-index warning for
/// find_dead_code, branching on whether warmup was attempted and what its
/// outcome was. The cargo-warm-but-LSP-cold case is the most common race
/// in practice (rust-analyzer's reference DB is populated async after
/// cargo check finishes) and deserves a specific message so the agent
/// retries instead of trusting the (correctly) empty result.
fn build_cold_warning_for_find_dead_code(
    checked: usize,
    warmup_report: Option<&crate::lsp::manager::WarmupReport>,
) -> String {
    let warmup_succeeded = warmup_report
        .map(|r| r.status == "warm" || (r.status == "cached" && r.cached_status.as_deref() == Some("warm")))
        .unwrap_or(false);

    if warmup_succeeded {
        format!(
            "LSP index appears cold despite warmup completing successfully. \
             {} exported symbol(s) checked, all returned zero references. \
             This is the known cargo-warm-but-LSP-cold race: cargo check finishing \
             does not guarantee rust-analyzer's reference database is queryable yet — \
             the index is populated async after cargo emits target-dir metadata, \
             typically 20-60s later. Retry this call in ~30s. Results in this \
             response are best-effort and every dead_symbol is marked confidence='low'.",
            checked
        )
    } else {
        format!(
            "LSP index appears cold: {} exported symbol(s) checked, all returned zero references. \
             Results are best-effort — every dead_symbol is marked confidence='low'. \
             Either retry after the language server finishes indexing, pass warmup=true \
             to actively warm the index, or pass require_warm_index=true to fail fast.",
            checked
        )
    }
}

/// CULTRA-948: find_references — semantic call-site lookup with role
/// classification. Anchors at `symbol`'s declaration in `file_path`, queries
/// LSP textDocument/references, and tags each hit with a role field so the
/// caller can tell a function call apart from a type reference or a doc
/// comment without re-scanning the source with Grep.
///
/// Inherits the CULTRA-947 cold-index guard pattern from find_dead_code:
/// when the references list is empty or declaration-only, lsp_index_status
/// is set to "cold" and a top-level `warning` field surfaces the issue.
/// Callers wanting to fail fast on cold indexes set require_warm_index=true.
fn find_references_tool(args: Map<String, Value>, server: &Server) -> Result<Value> {
    use crate::ast::parser::Parser;

    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: symbol"))?;

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;

    validate_file_exists(file_path, &server.workspace_root)?;

    let include_declaration = args
        .get("include_declaration")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    let require_warm_index = args
        .get("require_warm_index")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // CULTRA-950: optional active warmup, same semantics as find_dead_code.
    let do_warmup = args
        .get("warmup")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    // 1. Parse the anchor file and locate the symbol declaration.
    let parser = Parser::new();
    let file_context = parser.parse_file(file_path)
        .map_err(|e| anyhow!("Failed to parse file: {}", e))?;

    let warmup_report: Option<crate::lsp::manager::WarmupReport> = if do_warmup {
        Some(server.lsp.ensure_warm(&file_context.language.to_string(), std::path::Path::new(&file_context.file_path)))
    } else {
        None
    };

    let anchor = file_context.symbols.iter()
        .find(|s| s.name == symbol)
        .ok_or_else(|| anyhow!(
            "Symbol '{}' not found in {}. Use lsp_workspace_symbols to locate the file that declares it.",
            symbol, file_path
        ))?;

    // 2. Find the byte column of the symbol name on its declared line. LSP
    //    position queries take (line, character) and the call site in
    //    find_dead_code uses byte offsets — stay consistent.
    let anchor_contents = std::fs::read_to_string(&file_context.file_path)
        .map_err(|e| anyhow!("Failed to read file: {}", e))?;
    let anchor_lines: Vec<&str> = anchor_contents.lines().collect();
    let anchor_line_idx = anchor.line.saturating_sub(1) as usize;
    let anchor_col = anchor_lines
        .get(anchor_line_idx)
        .and_then(|l| l.find(symbol))
        .ok_or_else(|| anyhow!(
            "Could not locate symbol '{}' on declaration line {} of {}",
            symbol, anchor.line, file_path
        ))?;
    let lsp_line = anchor.line.saturating_sub(1);

    // 3. Query LSP references. An LSP failure (no server, missing binary,
    //    file not yet indexed) gets normalised to an empty list and falls
    //    through to the cold-index classifier below.
    let mut lsp_args = Map::new();
    lsp_args.insert("action".to_string(), json!("references"));
    lsp_args.insert("file_path".to_string(), json!(file_context.file_path));
    lsp_args.insert("line".to_string(), json!(lsp_line));
    lsp_args.insert("character".to_string(), json!(anchor_col as u32));
    // CULTRA-963/967: forward warmup to lsp_query so the retry loop fires.
    // This handles languages (TS) where ensure_warm finishes but the LSP
    // needs additional time to build its reference index.
    if do_warmup {
        lsp_args.insert("warmup".to_string(), json!(true));
    }

    let refs: Vec<Value> = match crate::lsp::tools::lsp_query(lsp_args, &server.lsp) {
        Ok(v) => v.get("references")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default(),
        Err(_) => Vec::new(),
    };

    // 4. Classify each reference. Cache per-file source reads so a function
    //    with 50 call sites in one file only reads that file once.
    let mut file_cache: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    let mut results: Vec<Value> = Vec::new();
    let mut any_non_definition = false;

    for loc in &refs {
        let uri = loc.get("uri").and_then(|u| u.as_str()).unwrap_or("");
        // file:// and file:/// both strip cleanly — the second slash stays.
        let ref_path: String = uri.strip_prefix("file://").unwrap_or(uri).to_string();

        let start = loc.get("range").and_then(|r| r.get("start"));
        let ref_line = start.and_then(|s| s.get("line")).and_then(|l| l.as_u64()).unwrap_or(0) as u32;
        let ref_col = start.and_then(|s| s.get("character")).and_then(|c| c.as_u64()).unwrap_or(0) as u32;

        // Anchor-declaration detection: same file + same LSP line.
        let is_def = ref_path.ends_with(&file_context.file_path) && ref_line == lsp_line;
        if !is_def {
            any_non_definition = true;
        }

        let context_line = file_cache
            .entry(ref_path.clone())
            .or_insert_with(|| {
                std::fs::read_to_string(&ref_path)
                    .map(|s| s.lines().map(String::from).collect())
                    .unwrap_or_default()
            })
            .get(ref_line as usize)
            .cloned()
            .unwrap_or_default();

        let role = classify_reference_role(&context_line, ref_col as usize, symbol.len(), is_def);

        // Default: skip the declaration unless the caller opted in. Most
        // refactor workflows only care about call sites.
        if is_def && !include_declaration {
            continue;
        }

        results.push(json!({
            "file": ref_path,
            "line": ref_line + 1,
            "col": ref_col + 1,
            "context": context_line.trim(),
            "role": role,
        }));
    }

    // 5. Cold-index classification. Mirrors find_dead_code (CULTRA-947):
    //    warm = at least one non-declaration reference exists; cold = empty
    //    or declaration-only. The "cold" signal on an empty list is
    //    conservative — a symbol with genuinely zero callers is
    //    indistinguishable from a cold index via one LSP call, so we flag
    //    both the same way and let the caller verify manually.
    let lsp_index_status = if refs.is_empty() {
        "cold"
    } else if any_non_definition {
        "warm"
    } else {
        "cold"
    };

    if lsp_index_status == "cold" && require_warm_index {
        return Err(anyhow!(
            "LSP index is cold for symbol '{}': {} reference(s) returned, none outside the declaration site. \
             This usually means the language server has not yet indexed cross-file callers. \
             Either wait for indexing to complete and retry, or pass require_warm_index=false \
             to accept best-effort results.",
            symbol, refs.len()
        ));
    }

    let mut response = json!({
        "symbol": symbol,
        "checked_at": format!("{}:{}:{}", file_context.file_path, anchor.line, anchor_col + 1),
        "total_references": results.len(),
        "results": results,
        "lsp_index_status": lsp_index_status,
        "role_buckets": ["definition", "call", "type_use", "doc", "unknown"],
        "caveats": [
            "Role classification is a fast heuristic on the character immediately following the symbol at each reference site: '(' → call; '{', '<', or '::' → type_use; doc-comment or inline-comment context → doc; else → unknown. Works well for Rust/Go/TS single-line call sites; fuzzy for macros, turbofish (foo::<T>() lands in type_use), multi-line calls, and /* */ block comments.",
            "Scope is workspace-wide — the tool returns every textDocument/references hit. There is no file/crate scope filter in v1.",
            "lsp_index_status (CULTRA-948, reusing the CULTRA-947 pattern): 'warm' means LSP returned at least one non-declaration reference. 'cold' means the references list was empty or declaration-only — usually an indexing gap, but also indistinguishable from a symbol that genuinely has no callers. Pass require_warm_index=true to fail fast on a cold index.",
            "Warmup race (CULTRA-952 follow-up): a successful warmup_report.status='warm' does NOT guarantee the LSP reference database is queryable on the very next call. cargo check / go build finishes before rust-analyzer / gopls finish populating their cross-file reference indexes — the gap is typically 20-60s. If this response shows lsp_index_status='cold' AND warmup_report.status='warm', the warning field will say so explicitly; retry in ~30s.",
            "Column positions are byte offsets, not UTF-16 code units. Matches the CULTRA LSP-backed tool convention but may drift on lines with multi-byte characters.",
        ],
    });

    if lsp_index_status == "cold" {
        let warning_text = build_cold_warning_for_find_references(symbol, refs.len(), warmup_report.as_ref());
        if let Value::Object(ref mut obj) = response {
            obj.insert("warning".to_string(), json!(warning_text));
        }
    }

    // CULTRA-950: surface the warmup report. Same shape as find_dead_code.
    if let Some(report) = warmup_report {
        if let Value::Object(ref mut obj) = response {
            obj.insert("warmup_report".to_string(), serde_json::to_value(&report).unwrap_or(Value::Null));
        }
    }

    Ok(response)
}

/// CULTRA-952 (post-verification): twin of build_cold_warning_for_find_dead_code,
/// for find_references. Same cargo-warm-but-LSP-cold race callout when
/// warmup succeeded but the reference query came back empty/decl-only.
fn build_cold_warning_for_find_references(
    symbol: &str,
    ref_count: usize,
    warmup_report: Option<&crate::lsp::manager::WarmupReport>,
) -> String {
    let warmup_succeeded = warmup_report
        .map(|r| r.status == "warm" || (r.status == "cached" && r.cached_status.as_deref() == Some("warm")))
        .unwrap_or(false);

    if warmup_succeeded {
        format!(
            "LSP index appears cold for symbol '{}' despite warmup completing successfully: \
             {} reference(s) returned, none outside the declaration. This is the known \
             cargo-warm-but-LSP-cold race: cargo check finishing does not guarantee \
             rust-analyzer's reference database is queryable yet — the index is populated \
             async after cargo emits target-dir metadata, typically 20-60s later. \
             Retry this call in ~30s.",
            symbol, ref_count
        )
    } else {
        format!(
            "LSP index appears cold for symbol '{}': {} reference(s) returned, none outside the declaration. \
             Results are best-effort. Either retry after the language server finishes indexing, \
             pass warmup=true to actively warm the index, or pass require_warm_index=true to fail fast.",
            symbol, ref_count
        )
    }
}

/// CULTRA-948: classify a single reference hit into one of five role buckets
/// by inspecting the source line. Cheap: one trim, one contains, one
/// post-symbol char lookup. Per Vestige's pushback, we ship 5 buckets on day
/// one rather than 3 — the refinement is cheap and the "other" catch-all would
/// have forced callers to grep through it manually to split struct literals
/// from doc comments.
fn classify_reference_role(line: &str, col: usize, symbol_len: usize, is_def: bool) -> &'static str {
    if is_def {
        return "definition";
    }

    // Doc / comment detection first — a symbol that *appears* inside a
    // comment shouldn't be mis-classified as a call just because the next
    // character happens to be '('. We check both line-start doc markers and
    // inline `//` comments that open before the symbol's column.
    let trimmed = line.trim_start();
    if trimmed.starts_with("///")
        || trimmed.starts_with("//!")
        || trimmed.starts_with("//")
        || trimmed.starts_with("/**")
        || trimmed.starts_with("/*")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("*/")
    {
        return "doc";
    }
    if let Some(prefix) = line.get(..col.min(line.len())) {
        if prefix.contains("//") {
            return "doc";
        }
    }

    // Post-symbol inspection. Find the first non-whitespace char after the
    // symbol; that single char is usually enough to disambiguate call from
    // type usage.
    let after_start = col.saturating_add(symbol_len);
    let after = line.get(after_start..).map(|s| s.trim_start()).unwrap_or("");
    let first = after.chars().next();

    match first {
        Some('(') => "call",
        Some('{') => "type_use",
        Some('<') => "type_use",
        Some(':') if after.starts_with("::") => "type_use",
        _ => "unknown",
    }
}

/// CULTRA-906: walk an exported function/method symbol list and attach a
/// `callers` array via LSP textDocument/references. Best-effort: any per-symbol
/// failure (LSP error, position lookup miss) yields a symbol without the field.
/// Default cap: top 10 callers per symbol.
fn enrich_symbols_with_callers(file_context: &crate::ast::types::FileContext, server: &Server) -> Value {
    use crate::mcp::types::{Scope, SymbolType};

    const MAX_CALLERS_PER_SYMBOL: usize = 10;

    // Read the file contents once so we can locate symbol-name columns.
    let file_contents = match std::fs::read_to_string(&file_context.file_path) {
        Ok(s) => s,
        Err(_) => {
            // Can't read the file; return symbols unchanged.
            return serde_json::to_value(&file_context.symbols).unwrap_or(json!([]));
        }
    };
    let lines: Vec<&str> = file_contents.lines().collect();

    let is_exported = |scope: &Scope| -> bool {
        matches!(scope, Scope::Public | Scope::Exported)
    };
    let is_callable = |t: &SymbolType| -> bool {
        matches!(t, SymbolType::Function | SymbolType::Method)
    };

    let mut out: Vec<Value> = Vec::with_capacity(file_context.symbols.len());
    for sym in &file_context.symbols {
        let mut sym_value = match serde_json::to_value(sym) {
            Ok(v) => v,
            Err(_) => continue,
        };

        // Only look up callers for exported callables. Skipping private and
        // non-callable symbols keeps cost bounded; the audit recommends this.
        if !is_callable(&sym.symbol_type) || !is_exported(&sym.scope) {
            out.push(sym_value);
            continue;
        }

        // Find column of the symbol name on its declared line.
        let line_idx = sym.line.saturating_sub(1) as usize;
        let column = lines.get(line_idx)
            .and_then(|line| line.find(sym.name.as_str()));
        let column = match column {
            Some(c) => c as u32,
            None => {
                // Couldn't locate the name on its line — skip enrichment.
                out.push(sym_value);
                continue;
            }
        };

        // LSP positions are 0-indexed.
        let lsp_line = sym.line.saturating_sub(1);

        let mut lsp_args = Map::new();
        lsp_args.insert("action".to_string(), json!("references"));
        lsp_args.insert("file_path".to_string(), json!(file_context.file_path));
        lsp_args.insert("line".to_string(), json!(lsp_line));
        lsp_args.insert("character".to_string(), json!(column));
        // CULTRA-964: pass warmup=true so the first symbol triggers
        // ensure_warm + the retry loop. Cached after the first call,
        // so subsequent symbols pay zero warmup cost.
        lsp_args.insert("warmup".to_string(), json!(true));

        match crate::lsp::tools::lsp_query(lsp_args, &server.lsp) {
            Ok(refs_value) => {
                let refs = refs_value.get("references")
                    .and_then(|v| v.as_array())
                    .cloned()
                    .unwrap_or_default();

                // Filter out the declaration site itself and cap to top N.
                let callers: Vec<Value> = refs.into_iter()
                    .filter_map(|loc| {
                        let uri = loc.get("uri")?.as_str()?.to_string();
                        let range = loc.get("range")?;
                        let start_line = range.get("start")?.get("line")?.as_u64()? as u32;
                        // Skip the declaration itself (same file, same line as definition).
                        if uri.ends_with(&file_context.file_path) && start_line == lsp_line {
                            return None;
                        }
                        Some(json!({
                            "uri": uri,
                            "line": start_line + 1, // 1-indexed for human consumption
                        }))
                    })
                    .take(MAX_CALLERS_PER_SYMBOL)
                    .collect();

                if let Value::Object(ref mut obj) = sym_value {
                    obj.insert("callers".to_string(), json!(callers));
                    obj.insert("caller_count".to_string(), json!(callers.len()));
                }
            }
            Err(_) => {
                // LSP unavailable or query failed — graceful degrade, no field added.
            }
        }

        out.push(sym_value);
    }

    Value::Array(out)
}

/// Tool implementation: analyze_file (consolidated from analyze_concurrency + analyze_react_component + analyze_css + css_variable_graph)
/// Validate analyzer name. Returns error for unknown values.
fn validate_analyzer(analyzer: &str) -> Result<()> {
    match analyzer {
        "concurrency" | "react" | "css" | "css_variables" | "security" | "complexity" => Ok(()),
        other => Err(anyhow!("Invalid analyzer '{}'. Must be 'concurrency', 'react', 'css', 'css_variables', 'security', or 'complexity'", other)),
    }
}

/// CULTRA-1066: filter knobs for the complexity analyzer. Each is optional
/// and applied independently; combining them is intersect-style (AND).
/// Summary aggregates are deliberately preserved (file-level metrics
/// shouldn't change based on view); only the `functions` list is filtered.
#[derive(Debug, Clone, Default)]
struct ComplexityFilters {
    min_cyclomatic: Option<u32>,
    min_cognitive: Option<u32>,
    top_n: Option<usize>,
}

impl ComplexityFilters {
    fn parse_from_args(args: &Map<String, Value>) -> Result<Self> {
        Ok(Self {
            min_cyclomatic: parse_positive_int(args, "min_cyclomatic", None, None)?
                .map(|n| n as u32),
            min_cognitive: parse_positive_int(args, "min_cognitive", None, None)?
                .map(|n| n as u32),
            top_n: parse_positive_int(args, "top_n", Some(1), None)?
                .map(|n| n as usize),
        })
    }

    fn is_active(&self) -> bool {
        self.min_cyclomatic.is_some() || self.min_cognitive.is_some() || self.top_n.is_some()
    }
}

/// Apply complexity filters to a single ComplexityAnalysis. Mutates and
/// returns. Idempotent if no filters are active. Functions are already
/// sorted by cyclomatic descending, so top_n just truncates.
fn apply_complexity_filters(
    mut analysis: ComplexityAnalysis,
    filters: &ComplexityFilters,
) -> ComplexityAnalysis {
    if let Some(min) = filters.min_cyclomatic {
        analysis.functions.retain(|f| f.cyclomatic >= min);
    }
    if let Some(min) = filters.min_cognitive {
        analysis.functions.retain(|f| f.cognitive >= min);
    }
    if let Some(n) = filters.top_n {
        analysis.functions.truncate(n);
    }
    analysis
}

/// Run a single analyzer against a single file. Pure helper used by both
/// analyze_file_tool and analyze_files_tool. Caller is responsible for
/// validating the analyzer name and the file path before calling.
/// CULTRA-1066: complexity_filters apply only to the "complexity" analyzer;
/// other analyzers ignore the parameter.
fn run_analyzer(
    analyzer: &str,
    file_path: &str,
    complexity_filters: &ComplexityFilters,
) -> Result<Value> {
    match analyzer {
        "concurrency" => {
            // CULTRA-910: dispatch by extension. .rs → Rust analyzer,
            // everything else → Go analyzer (the original implementation).
            if file_path.to_lowercase().ends_with(".rs") {
                let analysis = crate::ast::analyze_concurrency_rust(file_path)
                    .map_err(|e| anyhow!("Failed to analyze Rust concurrency: {}", e))?;
                return serde_json::to_value(analysis)
                    .map_err(|e| anyhow!("Failed to serialize result: {}", e));
            }
            let analysis = analyze_concurrency(file_path)
                .map_err(|e| anyhow!("Failed to analyze concurrency: {}", e))?;
            serde_json::to_value(analysis)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "react" => {
            let analysis = analyze_react_component(file_path)
                .map_err(|e| anyhow!("Failed to analyze React component: {}", e))?;
            serde_json::to_value(analysis)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "css" => {
            let analysis = analyze_css(file_path)
                .map_err(|e| anyhow!("Failed to analyze CSS: {}", e))?;
            serde_json::to_value(analysis)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "css_variables" => {
            let graph = css_variable_graph(file_path)
                .map_err(|e| anyhow!("Failed to build CSS variable graph: {}", e))?;
            serde_json::to_value(graph)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "security" => {
            let analysis = analyze_security(file_path)
                .map_err(|e| anyhow!("Failed to run security analysis: {}", e))?;
            serde_json::to_value(analysis)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "complexity" => {
            let analysis = analyze_complexity(file_path)
                .map_err(|e| anyhow!("Failed to run complexity analysis: {}", e))?;
            let analysis = if complexity_filters.is_active() {
                apply_complexity_filters(analysis, complexity_filters)
            } else {
                analysis
            };
            serde_json::to_value(analysis)
                .map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        _ => Err(anyhow!("analyzer already validated; this should be unreachable")),
    }
}

fn analyze_file_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let analyzer = args
        .get("analyzer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: analyzer"))?;
    validate_analyzer(analyzer)?;

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;
    validate_file_exists(file_path, workspace_root)?;

    // CULTRA-1066: complexity filters (no-op for non-complexity analyzers)
    let complexity_filters = ComplexityFilters::parse_from_args(&args)?;

    run_analyzer(analyzer, file_path, &complexity_filters)
}

/// CULTRA-949: analyze_symbol — filter analyze_file(complexity)'s output to a
/// single function so a refactor loop can cheaply check "did my edit move the
/// needle?" without re-ingesting ~100 functions of JSON every iteration.
///
/// v1 supports analyzer="complexity" only. The implementation runs the full
/// analyzer (dominated by the tree-sitter parse) then filters the results
/// in-process — strictly cheaper than writing a parallel one-symbol analyzer
/// and impossible to drift out of sync with analyze_file.
///
/// Delta mode: when delta_against is passed as an inline object with any of
/// {cyclomatic, cognitive, lines, rating}, the corresponding metric is
/// rendered as {prev, now, delta} instead of a scalar. This is purely a
/// presentation layer — no server-side baseline storage, per the task spec.
fn analyze_symbol_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let analyzer = args
        .get("analyzer")
        .and_then(|v| v.as_str())
        .unwrap_or("complexity");
    if analyzer != "complexity" {
        return Err(anyhow!(
            "analyze_symbol v1 only supports analyzer='complexity' (got '{}'). \
             Other analyzers will be added if the pattern recurs.",
            analyzer
        ));
    }

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;
    validate_file_exists(file_path, workspace_root)?;

    let symbol = args
        .get("symbol")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: symbol"))?;

    let analysis = analyze_complexity(file_path)
        .map_err(|e| anyhow!("Failed to run complexity analysis: {}", e))?;

    // Filter to functions named `symbol`. Preserves the order from the
    // analyzer walker so the first match is deterministic (first occurrence
    // in source).
    let matches: Vec<_> = analysis.functions.iter()
        .filter(|f| f.name == symbol)
        .collect();

    if matches.is_empty() {
        let available: Vec<&str> = analysis.functions.iter()
            .map(|f| f.name.as_str())
            .collect();
        return Err(anyhow!(
            "Symbol '{}' not found in {} (analyzer: complexity). \
             Available functions ({}): {}. \
             Check spelling or run analyze_file for the full list.",
            symbol,
            file_path,
            available.len(),
            if available.is_empty() { "<none>".to_string() } else { available.join(", ") }
        ));
    }

    let first = matches[0];
    let overload_warning = if matches.len() > 1 {
        Some(format!(
            "Multiple functions named '{}' found ({} total) — returning the first match at {}. \
             analyze_symbol v1 does not disambiguate by receiver/signature.",
            symbol, matches.len(), first.location
        ))
    } else {
        None
    };

    // Delta-against-baseline rendering. Each metric's baseline is optional:
    // a baseline with only `cyclomatic` renders just that metric as a delta
    // and the rest as scalars.
    let baseline = args.get("delta_against").and_then(|v| v.as_object());
    let delta_mode = baseline.is_some();

    let mk_delta = |prev: Option<i64>, now: u32| -> Value {
        match prev {
            Some(p) => json!({"prev": p, "now": now, "delta": (now as i64) - p}),
            None => json!(now),
        }
    };

    let (cyclomatic_v, cognitive_v, lines_v, rating_v) = if let Some(b) = baseline {
        let cyc = mk_delta(b.get("cyclomatic").and_then(|v| v.as_i64()), first.cyclomatic);
        let cog = mk_delta(b.get("cognitive").and_then(|v| v.as_i64()), first.cognitive);
        let lin = mk_delta(b.get("lines").and_then(|v| v.as_i64()), first.lines);
        let rat = match b.get("rating").and_then(|v| v.as_str()) {
            Some(prev) => json!({"prev": prev, "now": first.rating.clone()}),
            None => json!(first.rating.clone()),
        };
        (cyc, cog, lin, rat)
    } else {
        (
            json!(first.cyclomatic),
            json!(first.cognitive),
            json!(first.lines),
            json!(first.rating.clone()),
        )
    };

    let mut response = json!({
        "analyzer": "complexity",
        "file_path": analysis.file_path,
        "language": analysis.language,
        "name": first.name,
        "location": first.location,
        "line_start": first.line_start,
        "line_end": first.line_end,
        "cyclomatic": cyclomatic_v,
        "cognitive": cognitive_v,
        "lines": lines_v,
        "rating": rating_v,
        "delta_mode": delta_mode,
    });

    if let Value::Object(ref mut obj) = response {
        if let Some(recv) = &first.receiver {
            obj.insert("receiver".to_string(), json!(recv));
        }
        if let Some(w) = overload_warning {
            obj.insert("warning".to_string(), json!(w));
        }
    }

    Ok(response)
}

/// CULTRA-905: analyze_files (plural) — bulk analyzer over many files in parallel.
/// Uses std::thread::scope so per-file failures don't poison other workers and
/// the result vector preserves input order. Returns:
///   [{file_path, success, result?, error?}, ...]
fn analyze_files_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let analyzer = args
        .get("analyzer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: analyzer"))?;
    validate_analyzer(analyzer)?;

    let file_paths_value = args
        .get("file_paths")
        .ok_or_else(|| anyhow!("Missing required parameter: file_paths"))?;
    let file_paths_arr = file_paths_value
        .as_array()
        .ok_or_else(|| anyhow!("file_paths must be an array of strings"))?;
    if file_paths_arr.is_empty() {
        return Err(anyhow!("file_paths must contain at least one entry"));
    }
    if file_paths_arr.len() > 500 {
        return Err(anyhow!("file_paths capped at 500 entries per call (got {})", file_paths_arr.len()));
    }

    // Validate every entry up front. Path validation is cheap (filesystem
    // stat) and doing it serially before spawning threads keeps error
    // messages deterministic.
    let mut file_paths: Vec<String> = Vec::with_capacity(file_paths_arr.len());
    for (i, v) in file_paths_arr.iter().enumerate() {
        let s = v.as_str().ok_or_else(|| anyhow!("file_paths[{}] is not a string", i))?;
        file_paths.push(s.to_string());
    }

    // CULTRA-1066: complexity filters (no-op for non-complexity analyzers)
    let complexity_filters = ComplexityFilters::parse_from_args(&args)?;

    // Cap concurrency so a 500-file batch doesn't spawn 500 threads.
    // 8 is a sensible default for CPU-bound tree-sitter parsing on most hosts.
    let max_workers = 8usize.min(file_paths.len());
    let chunk_size = (file_paths.len() + max_workers - 1) / max_workers;
    let analyzer_owned = analyzer.to_string();
    let workspace_root_owned: std::path::PathBuf = workspace_root.to_path_buf();
    let filters_owned = complexity_filters.clone();

    // Each entry is (index, FileResult). Output is sorted back to input order.
    let mut results: Vec<(usize, Value)> = Vec::with_capacity(file_paths.len());

    std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(max_workers);
        for chunk_idx in 0..max_workers {
            let start = chunk_idx * chunk_size;
            if start >= file_paths.len() {
                break;
            }
            let end = std::cmp::min(start + chunk_size, file_paths.len());
            let chunk: Vec<(usize, String)> = (start..end)
                .map(|i| (i, file_paths[i].clone()))
                .collect();
            let analyzer_ref = &analyzer_owned;
            let workspace_ref = &workspace_root_owned;
            let filters_ref = &filters_owned;
            handles.push(scope.spawn(move || {
                let mut local: Vec<(usize, Value)> = Vec::with_capacity(chunk.len());
                for (idx, path) in chunk {
                    let entry = match validate_file_exists(&path, workspace_ref) {
                        Err(e) => json!({
                            "file_path": path,
                            "success": false,
                            "error": format!("{}", e),
                        }),
                        Ok(_) => match run_analyzer(analyzer_ref, &path, filters_ref) {
                            Ok(v) => json!({
                                "file_path": path,
                                "success": true,
                                "result": v,
                            }),
                            Err(e) => json!({
                                "file_path": path,
                                "success": false,
                                "error": format!("{}", e),
                            }),
                        },
                    };
                    local.push((idx, entry));
                }
                local
            }));
        }
        for h in handles {
            if let Ok(local) = h.join() {
                results.extend(local);
            }
        }
    });

    // Restore input order.
    results.sort_by_key(|(i, _)| *i);
    let ordered: Vec<Value> = results.into_iter().map(|(_, v)| v).collect();

    let total = ordered.len();
    let succeeded = ordered.iter().filter(|v| v.get("success").and_then(|s| s.as_bool()).unwrap_or(false)).count();

    Ok(json!({
        "analyzer": analyzer,
        "total": total,
        "succeeded": succeeded,
        "failed": total - succeeded,
        "results": ordered,
    }))
}

/// Tool implementation: find_interface_implementations
fn find_interface_implementations_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?;

    validate_file_exists(file_path, workspace_root)?;

    let interface_name = args
        .get("interface_name")
        .and_then(|v| v.as_str());

    let analysis = find_interface_implementations(file_path, interface_name)
        .map_err(|e| anyhow!("Failed to find interface implementations: {}", e))?;

    serde_json::to_value(analysis)
        .map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

// ========== CSS Analysis Tools ==========

/// Tool implementation: find_css_rules
fn find_css_rules_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?;

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing pattern"))?;

    validate_file_exists(file_path, workspace_root)?;

    let rules = find_css_rules(file_path, pattern)
        .map_err(|e| anyhow!("Failed to find CSS rules: {}", e))?;

    serde_json::to_value(rules)
        .map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

/// Tool implementation: find_unused_selectors
fn find_unused_selectors_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let css_path = args
        .get("css_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing css_path"))?;

    let component_dir = args
        .get("component_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing component_dir"))?;

    validate_file_exists(css_path, workspace_root)?;

    // Walk component_dir for matching files
    let component_paths = collect_component_files(component_dir)?;
    let path_refs: Vec<&str> = component_paths.iter().map(|s| s.as_str()).collect();

    let unused = find_unused_selectors(css_path, &path_refs)
        .map_err(|e| anyhow!("Failed to find unused selectors: {}", e))?;

    serde_json::to_value(unused)
        .map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

/// Walk a directory and collect .tsx/.ts/.jsx/.js/.html file paths
fn collect_component_files(dir: &str) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let extensions = ["tsx", "ts", "jsx", "js", "html"];

    fn walk(path: &std::path::Path, extensions: &[&str], files: &mut Vec<String>) {
        if path.is_dir() {
            // Skip common non-source directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(name, "node_modules" | ".git" | "dist" | "build" | "target" | ".next" | "__pycache__" | ".venv" | "vendor") {
                    return;
                }
            }
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    walk(&entry.path(), extensions, files);
                }
            }
        } else if path.is_file() {
            if let Some(ext) = path.extension().and_then(|e| e.to_str()) {
                if extensions.contains(&ext) {
                    if let Some(p) = path.to_str() {
                        files.push(p.to_string());
                    }
                }
            }
        }
    }

    walk(std::path::Path::new(dir), &extensions, &mut files);
    Ok(files)
}

// ========== CSS Analysis Tools V2 ==========

/// Tool implementation: resolve_tailwind_classes
fn resolve_tailwind_classes_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    let classes_val = args
        .get("classes")
        .ok_or_else(|| anyhow!("Missing required parameter: classes"))?;

    let classes: Vec<String> = classes_val
        .as_array()
        .ok_or_else(|| anyhow!("Parameter 'classes' must be an array of strings"))?
        .iter()
        .filter_map(|v| v.as_str().map(|s| s.to_string()))
        .collect();

    let css_path = args.get("css_path").and_then(|v| v.as_str());

    if let Some(path) = css_path {
        validate_file_exists(path, workspace_root)?;
    }

    let class_refs: Vec<&str> = classes.iter().map(|s| s.as_str()).collect();
    let result = resolve_tailwind_classes(&class_refs, css_path)
        .map_err(|e| anyhow!("Failed to resolve Tailwind classes: {}", e))?;

    serde_json::to_value(result)
        .map_err(|e| anyhow!("Failed to serialize result: {}", e))
}


// ========== Engine V2 Intelligence Tools ==========

/// Tool implementation: search_code_context
/// CULTRA-940: collected filter parameters for search_code_context.
/// All Options so "unset" means "no filter".
struct SymbolFilters<'a> {
    symbol_name: Option<&'a str>,
    symbol_type: Option<&'a str>,
    receiver: Option<&'a str>,
    scope: Option<&'a str>,
    calls: Option<&'a str>,
}

impl<'a> SymbolFilters<'a> {
    fn from_args(args: &'a Map<String, Value>) -> Self {
        Self {
            symbol_name: args.get("symbol_name").and_then(|v| v.as_str()),
            symbol_type: args.get("symbol_type").and_then(|v| v.as_str()),
            receiver: args.get("receiver").and_then(|v| v.as_str()),
            scope: args.get("scope").and_then(|v| v.as_str()),
            calls: args.get("calls").and_then(|v| v.as_str()),
        }
    }

    /// True iff every active filter matches. A filter is inactive (passes) when None.
    fn matches(&self, sym: &Map<String, Value>) -> bool {
        if let Some(f) = self.symbol_name {
            if !sym_field_contains_ci(sym, "name", f) { return false; }
        }
        if let Some(f) = self.symbol_type {
            if !sym_field_eq(sym, "type", f) { return false; }
        }
        if let Some(f) = self.receiver {
            if !sym_field_eq(sym, "receiver", f) { return false; }
        }
        if let Some(f) = self.scope {
            if !sym_field_eq(sym, "scope", f) { return false; }
        }
        if let Some(f) = self.calls {
            if !sym_calls_contain(sym, f) { return false; }
        }
        true
    }
}

fn sym_field_contains_ci(sym: &Map<String, Value>, field: &str, needle: &str) -> bool {
    sym.get(field)
        .and_then(|v| v.as_str())
        .map(|v| v.to_lowercase().contains(&needle.to_lowercase()))
        .unwrap_or(false)
}

fn sym_field_eq(sym: &Map<String, Value>, field: &str, expected: &str) -> bool {
    sym.get(field).and_then(|v| v.as_str()) == Some(expected)
}

fn sym_calls_contain(sym: &Map<String, Value>, needle: &str) -> bool {
    sym.get("calls")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().any(|c| c.as_str().map(|s| s.contains(needle)).unwrap_or(false)))
        .unwrap_or(false)
}

fn ctx_matches_file_pattern(ctx: &Map<String, Value>, pattern: &str) -> bool {
    ctx.get("file_path")
        .and_then(|v| v.as_str())
        .map(|fp| fp.contains(pattern))
        .unwrap_or(true) // missing file_path → don't filter
}

fn search_code_context(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    // Get code context from latest session via API
    let code_context: Value = api_get_with_filters(
        server,
        "/api/v2/sessions/code-context",
        &["project_id"],
        &[],
        &args,
    )?;

    let filters = SymbolFilters::from_args(&args);
    let file_pattern = args.get("file_pattern").and_then(|v| v.as_str());
    let limit = parse_positive_int(&args, "limit", Some(1), Some(500))?.unwrap_or(50) as usize;

    let mut results: Vec<Value> = Vec::new();

    for ctx in code_context.as_array().into_iter().flatten() {
        if results.len() >= limit { break; }
        let Some(ctx_obj) = ctx.as_object() else { continue; };

        if let Some(pattern) = file_pattern {
            if !ctx_matches_file_pattern(ctx_obj, pattern) { continue; }
        }

        let symbols = match ctx_obj.get("symbols").and_then(|v| v.as_array()) {
            Some(arr) => arr,
            None => continue,
        };

        for sym in symbols {
            if results.len() >= limit { break; }
            let Some(sym_obj) = sym.as_object() else { continue; };
            if filters.matches(sym_obj) {
                results.push(sym.clone());
            }
        }
    }

    Ok(json!({
        "success": true,
        "total_found": results.len(),
        "results": results
    }))
}

/// Tool implementation: read_symbol_lines
fn read_symbol_lines(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let (file_path, start_line, end_line) = if let Some(location) = args.get("location").and_then(|v| v.as_str()) {
        // Parse location string "file.go:57-130" or "file.go:57"
        split_location(location)?
    } else {
        // Use explicit parameters
        let file_path = args
            .get("file_path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow!("file_path or location is required"))?
            .to_string();

        let start_line = args
            .get("start_line")
            .and_then(|v| v.as_i64())
            .ok_or_else(|| anyhow!("start_line is required when using file_path"))? as usize;

        let end_line = args
            .get("end_line")
            .and_then(|v| v.as_i64())
            .map(|v| v as usize)
            .unwrap_or(start_line);

        (file_path, start_line, end_line)
    };

    // Validate file exists, is readable, and is within workspace
    validate_file_exists(&file_path, workspace_root)?;

    // Read file lines
    let file = File::open(&file_path)
        .map_err(|e| anyhow!("Failed to open file {}: {}", file_path, e))?;

    let reader = BufReader::new(file);
    let mut content = Vec::new();
    let mut current_line = 1;

    for line in reader.lines() {
        let line = line.map_err(|e| anyhow!("Failed to read line: {}", e))?;

        if current_line >= start_line && current_line <= end_line {
            content.push(format!("{:5}→{}", current_line, line));
        }

        if current_line > end_line {
            break;
        }

        current_line += 1;
    }

    Ok(json!({
        "success": true,
        "file_path": file_path,
        "start_line": start_line,
        "end_line": end_line,
        "content": content.join("\n")
    }))
}

/// Tool implementation: init_vector_db
fn init_vector_db(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_post(server, "/api/v2/vector/init", args)
}

/// Tool implementation: query_context
fn query_context(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let query = args
        .get("query")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: query"))?;

    // Validate numeric parameters with ranges
    let limit = parse_positive_int(&args, "limit", Some(1), Some(20))?.unwrap_or(5);
    let graph_depth = parse_positive_int(&args, "graph_depth", Some(1), Some(5))?.unwrap_or(2);

    // Build request body with all parameters
    let body = json!({
        "project_id": project_id,
        "query": query,
        "limit": limit,
        "min_score": args.get("min_score").and_then(|v| v.as_f64()).unwrap_or(0.7),
        "include_graph": args.get("include_graph").and_then(|v| v.as_bool()).unwrap_or(false),
        "graph_depth": graph_depth,
        "use_retrievability": args.get("use_retrievability").and_then(|v| v.as_bool()).unwrap_or(false)
    });

    // unwrap: body is a json!({...}) literal — always Value::Object.
    api_post(server, "/api/v2/vector/query", body.as_object().unwrap().clone())
}

/// Tool implementation: add_graph_edge
fn add_graph_edge(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let required_fields = ["from_type", "from_id", "to_type", "to_id", "edge_type", "project_id"];
    for field in &required_fields {
        if !args.contains_key(*field) || args.get(*field).and_then(|v| v.as_str()).is_none() {
            return Err(anyhow!("Missing required parameter: {}", field));
        }
    }
    api_post(server, "/api/v2/graph/edges", args)
}

/// Tool implementation: query_graph
fn query_graph(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let _entity_type = args
        .get("entity_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: entity_type"))?;

    let _entity_id = args
        .get("entity_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: entity_id"))?;

    api_get_with_filters(
        server,
        "/api/v2/graph/edges",
        &["project_id", "entity_type", "entity_id"],
        &["edge_type", "direction", "depth"],
        &args,
    )
}

/// Tool implementation: get_graph_neighbors
fn get_graph_neighbors(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: project_id"))?;

    let _entity_type = args
        .get("entity_type")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: entity_type"))?;

    let _entity_id = args
        .get("entity_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: entity_id"))?;

    api_get_with_filters(
        server,
        "/api/v2/graph/neighbors",
        &["project_id", "entity_type", "entity_id"],
        &["edge_classes"],
        &args,
    )
}

// ========== Helper Functions for read_symbol_lines ==========

/// Split location string "file.go:57-130" into (file_path, start_line, end_line)
fn split_location(location: &str) -> Result<(String, usize, usize)> {
    // Find the LAST colon - handles Windows paths like C:\path:42
    let colon_pos = location.rfind(':')
        .ok_or_else(|| anyhow!("Invalid location format '{}' - expected 'file:line' or 'file:start-end'", location))?;
    let file_path = location[..colon_pos].to_string();
    let line_part = &location[colon_pos + 1..];

    if line_part.contains('-') {
        // Range format: "57-130"
        let range: Vec<&str> = line_part.splitn(2, '-').collect();
        if range.len() != 2 {
            return Err(anyhow!("Invalid line range format"));
        }

        let start_line = range[0].parse::<usize>()
            .map_err(|_| anyhow!("Invalid start line number"))?;
        let end_line = range[1].parse::<usize>()
            .map_err(|_| anyhow!("Invalid end line number"))?;

        Ok((file_path, start_line, end_line))
    } else {
        // Single line: "57"
        let line = line_part.parse::<usize>()
            .map_err(|_| anyhow!("Invalid line number"))?;
        Ok((file_path, line, line))
    }
}

// ============================================================================
// Tool: contextual_search — grep + AST symbol annotation (CULTRA-995)
// ============================================================================

/// Search for a text pattern and annotate each match with the containing AST symbol.
/// Combines ripgrep-style text search with parse_file_ast-level context.
fn contextual_search_tool(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    use crate::ast::parser::Parser;
    use std::process::Command;

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: pattern"))?;

    // Resolve search path: relative paths join with workspace_root, absolute
    // paths are used as-is. Default to workspace_root when no path is given.
    // (CULTRA-1067: previously rejected relative paths with a misleading
    // "must be within the workspace root" error.)
    let provided = args.get("path").and_then(|v| v.as_str());
    let resolved_path = match provided {
        Some(p) => {
            let pb = std::path::PathBuf::from(p);
            if pb.is_absolute() {
                pb
            } else {
                workspace_root.join(pb)
            }
        }
        None => workspace_root.to_path_buf(),
    };

    // Canonicalize for the security check (defeats `..` traversal and symlink
    // escapes). Mirror the validate_file_exists pattern.
    let canonical_path = resolved_path.canonicalize().map_err(|e| {
        anyhow!(
            "path '{}' does not exist or cannot be accessed: {}",
            resolved_path.display(),
            e
        )
    })?;
    let canonical_workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(anyhow!(
            "path must be within workspace; got '{}', workspace root is '{}'",
            resolved_path.display(),
            workspace_root.display()
        ));
    }
    let search_path = canonical_path;

    let file_glob = args.get("glob").and_then(|v| v.as_str());
    let max_matches = args
        .get("max_results")
        .and_then(|v| v.as_u64())
        .unwrap_or(100) as usize;

    // Step 1: grep for the pattern using ripgrep (preferred) or grep (fallback)
    let has_rg = Command::new("rg").arg("--version")
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status().is_ok();
    let mut cmd = if has_rg {
        let mut c = Command::new("rg");
        c.args(["--line-number", "--no-heading", "--with-filename", "--max-count", "50"]);
        if let Some(glob) = file_glob {
            c.args(["--glob", glob]);
        }
        c.arg(pattern);
        c.arg(&search_path);
        c
    } else {
        let mut c = Command::new("grep");
        c.args(["-rn", "--include"]);
        if let Some(glob) = file_glob {
            c.arg(glob);
        } else {
            c.arg("*");
        }
        c.arg(pattern);
        c.arg(&search_path);
        c
    };

    let output = cmd
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|e| anyhow!("Failed to run search: {}", e))?;

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Step 2: parse grep output (file:line:text format)
    struct GrepHit {
        file: String,
        line: u32,
        text: String,
    }

    let mut hits: Vec<GrepHit> = Vec::new();
    for line in stdout.lines() {
        if hits.len() >= max_matches { break; }
        // Parse "file:line:text" — handle Windows paths with drive letters
        let parts: Vec<&str> = line.splitn(3, ':').collect();
        if parts.len() >= 3 {
            if let Ok(line_num) = parts[1].parse::<u32>() {
                hits.push(GrepHit {
                    file: parts[0].to_string(),
                    line: line_num,
                    text: parts[2].to_string(),
                });
            }
        }
    }

    if hits.is_empty() {
        return Ok(json!({
            "matches": [],
            "total": 0,
            "pattern": pattern,
        }));
    }

    // Step 3: group by file and parse AST for each unique file
    let parser = Parser::new();
    let mut ast_cache: std::collections::HashMap<String, Vec<(String, String, u32, u32)>> = std::collections::HashMap::new();

    for hit in &hits {
        if ast_cache.contains_key(&hit.file) { continue; }
        // Parse the file's AST to get symbol ranges
        let symbols: Vec<(String, String, u32, u32)> = match parser.parse_file(&hit.file) {
            Ok(ctx) => ctx.symbols.iter().map(|s| {
                (s.name.clone(), format!("{:?}", s.symbol_type).to_lowercase(), s.line, s.end_line)
            }).collect(),
            Err(_) => Vec::new(), // Unsupported language — no symbol context
        };
        ast_cache.insert(hit.file.clone(), symbols);
    }

    // Step 4: annotate each hit with the containing symbol
    let mut matches: Vec<Value> = Vec::new();
    // ripgrep results will be under the canonical search root, so strip
    // against canonical_workspace; fall back to workspace_root for the case
    // where the two are identical (no symlinks).
    let workspace_str = workspace_root.to_string_lossy();
    let canonical_str = canonical_workspace.to_string_lossy();

    for hit in &hits {
        let relative_file = hit.file
            .strip_prefix(canonical_str.as_ref())
            .or_else(|| hit.file.strip_prefix(workspace_str.as_ref()))
            .unwrap_or(&hit.file)
            .trim_start_matches('/');

        let containing_symbol = ast_cache.get(&hit.file)
            .and_then(|symbols| {
                // Find the innermost symbol containing this line
                symbols.iter()
                    .filter(|(_, _, start, end)| hit.line >= *start && hit.line <= *end)
                    .max_by_key(|(_, _, start, _)| *start) // innermost = highest start line
                    .map(|(name, sym_type, start, end)| {
                        json!({
                            "name": name,
                            "type": sym_type,
                            "line": start,
                            "end_line": end,
                        })
                    })
            });

        let mut entry = json!({
            "file": relative_file,
            "line": hit.line,
            "text": hit.text.trim(),
        });
        if let Some(sym) = containing_symbol {
            entry.as_object_mut().unwrap().insert("containing_symbol".to_string(), sym);
        }
        matches.push(entry);
    }

    let total = matches.len();
    Ok(json!({
        "matches": matches,
        "total": total,
        "pattern": pattern,
        "files_searched": ast_cache.len(),
    }))
}

// ============================================================================
// Tool: project_info — structured manifest reader (CULTRA-996)
// ============================================================================

fn project_info_tool(args: Map<String, Value>, server: &Server) -> Result<Value> {
    project_info_tool_inner(args, &server.workspace_root)
}

/// Inner impl of project_info_tool that takes the workspace_root directly so
/// it's testable without a full Server. (CULTRA-1070)
fn project_info_tool_inner(args: Map<String, Value>, workspace_root: &std::path::Path) -> Result<Value> {
    // CULTRA-1070: same resolve+canonicalize pattern as contextual_search_tool
    // (CULTRA-1067). Relative paths join with workspace_root; canonicalization
    // defeats `..` traversal and symlink escapes; nonexistent paths get a clear
    // error. Replaces lexical-only `starts_with` check that misclassified relative
    // paths as workspace-escape and missed real escapes.
    let provided = args.get("path").and_then(|v| v.as_str());
    let resolved_path = match provided {
        Some(p) => {
            let pb = std::path::PathBuf::from(p);
            if pb.is_absolute() {
                pb
            } else {
                workspace_root.join(pb)
            }
        }
        None => workspace_root.to_path_buf(),
    };

    let canonical_path = resolved_path.canonicalize().map_err(|e| {
        anyhow!(
            "path '{}' does not exist or cannot be accessed: {}",
            resolved_path.display(),
            e
        )
    })?;
    let canonical_workspace = workspace_root
        .canonicalize()
        .unwrap_or_else(|_| workspace_root.to_path_buf());
    if !canonical_path.starts_with(&canonical_workspace) {
        return Err(anyhow!(
            "path must be within workspace; got '{}', workspace root is '{}'",
            resolved_path.display(),
            workspace_root.display()
        ));
    }
    let project_path = canonical_path;

    if !project_path.is_dir() {
        return Err(anyhow!("Path '{}' is not a directory", project_path.display()));
    }

    // Detect manifest type by checking for known files
    let manifests = [
        ("Cargo.toml", "rust", "cargo"),
        ("pyproject.toml", "python", "uv"),
        ("package.json", "javascript", "npm"),
        ("go.mod", "go", "go"),
        ("composer.json", "php", "composer"),
    ];

    for (filename, language, default_pm) in &manifests {
        let manifest_path = project_path.join(filename);
        if manifest_path.exists() {
            let content = std::fs::read_to_string(&manifest_path)
                .map_err(|e| anyhow!("Failed to read {}: {}", filename, e))?;

            let info = match *language {
                "rust" => parse_cargo_toml(&content, &project_path),
                "python" => parse_pyproject_toml(&content, &project_path),
                "javascript" => parse_package_json(&content, &project_path),
                "go" => parse_go_mod(&content, &project_path),
                "php" => parse_composer_json(&content, &project_path),
                _ => json!({"language": language}),
            };

            let mut result = json!({
                "language": language,
                "manifest": filename,
                "manifest_path": manifest_path.to_string_lossy(),
                "package_manager": default_pm,
            });

            // Merge parsed info
            if let (Value::Object(ref mut base), Value::Object(parsed)) = (&mut result, info) {
                for (k, v) in parsed {
                    base.insert(k, v);
                }
            }

            // Check LSP availability via PATH
            let lsp_binary = match *language {
                "rust" => "rust-analyzer",
                "python" => "pyright-langserver",
                "javascript" => "typescript-language-server",
                "go" => "gopls",
                "php" => "intelephense",
                _ => "",
            };
            if !lsp_binary.is_empty() {
                let available = std::process::Command::new(lsp_binary)
                    .arg("--version")
                    .stdout(std::process::Stdio::null())
                    .stderr(std::process::Stdio::null())
                    .status()
                    .is_ok();
                result.as_object_mut().unwrap().insert("lsp".to_string(), json!({
                    "server": lsp_binary,
                    "available": available,
                }));
            }

            // Count source files
            let extensions: &[&str] = match *language {
                "rust" => &["rs"],
                "python" => &["py"],
                "javascript" => &["js", "jsx", "ts", "tsx"],
                "go" => &["go"],
                "php" => &["php"],
                _ => &[],
            };
            let (source_count, test_count) = count_project_files(&project_path, extensions);
            result.as_object_mut().unwrap().insert("files".to_string(), json!({
                "source": source_count,
                "test": test_count,
            }));

            return Ok(result);
        }
    }

    Err(anyhow!("No recognized manifest file found in '{}'", project_path.display()))
}


fn parse_cargo_toml(content: &str, project_path: &std::path::Path) -> Value {
    let mut deps = Vec::new();
    let mut dev_deps = Vec::new();
    let mut name = String::new();
    let mut version = String::new();
    let mut edition = String::new();
    let mut rust_version = String::new();

    let mut section = "";
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            section = if trimmed.starts_with("[package]") { "package" }
                else if trimmed.starts_with("[dependencies]") { "deps" }
                else if trimmed.starts_with("[dev-dependencies]") { "dev-deps" }
                else { "other" };
            continue;
        }
        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"');
            match section {
                "package" => match key {
                    "name" => name = val.to_string(),
                    "version" => version = val.to_string(),
                    "edition" => edition = val.to_string(),
                    "rust-version" => rust_version = val.to_string(),
                    _ => {}
                },
                "deps" => deps.push(key.to_string()),
                "dev-deps" => dev_deps.push(key.to_string()),
                _ => {}
            }
        }
    }

    let has_workspace = project_path.join("Cargo.lock").exists();

    json!({
        "name": name,
        "version": version,
        "edition": edition,
        "rust_version": rust_version,
        "dependencies": deps,
        "dev_dependencies": dev_deps,
        "has_lockfile": has_workspace,
    })
}

fn parse_pyproject_toml(content: &str, project_path: &std::path::Path) -> Value {
    let mut deps = Vec::new();
    let mut dev_deps = Vec::new();
    let mut name = String::new();
    let mut version = String::new();
    let mut python_version = String::new();
    let mut in_deps = false;
    let mut in_dev_deps = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('[') {
            in_deps = false;
            in_dev_deps = false;
        }
        if trimmed == "dependencies = [" || trimmed.starts_with("dependencies = [") {
            in_deps = true;
            // Handle inline single-line arrays
            if trimmed.contains(']') {
                in_deps = false;
            }
            continue;
        }
        if trimmed.contains("dev-dependencies") || trimmed.contains("dev.dependencies") {
            in_dev_deps = true;
            continue;
        }
        if trimmed == "]" {
            in_deps = false;
            in_dev_deps = false;
            continue;
        }

        if in_deps {
            let dep = trimmed.trim_matches(',').trim().trim_matches('"').to_string();
            if !dep.is_empty() { deps.push(dep); }
        } else if in_dev_deps {
            if let Some((key, _)) = trimmed.split_once('=') {
                dev_deps.push(key.trim().trim_matches('"').to_string());
            }
        }

        // Package metadata
        if let Some((key, val)) = trimmed.split_once('=') {
            let key = key.trim();
            let val = val.trim().trim_matches('"');
            match key {
                "name" => if name.is_empty() { name = val.to_string(); },
                "version" => if version.is_empty() { version = val.to_string(); },
                "requires-python" => python_version = val.to_string(),
                _ => {}
            }
        }
    }

    let pm = if project_path.join("uv.lock").exists() { "uv" }
        else if project_path.join("poetry.lock").exists() { "poetry" }
        else if project_path.join("Pipfile.lock").exists() { "pipenv" }
        else { "pip" };

    let has_venv = project_path.join(".venv").exists() || project_path.join("venv").exists();

    json!({
        "name": name,
        "version": version,
        "python_version": python_version,
        "package_manager": pm,
        "dependencies": deps,
        "dev_dependencies": dev_deps,
        "has_venv": has_venv,
    })
}

fn parse_package_json(content: &str, project_path: &std::path::Path) -> Value {
    let parsed: Value = serde_json::from_str(content).unwrap_or(json!({}));
    let obj = parsed.as_object();

    let name = obj.and_then(|o| o.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let version = obj.and_then(|o| o.get("version")).and_then(|v| v.as_str()).unwrap_or("").to_string();

    let deps: Vec<String> = obj.and_then(|o| o.get("dependencies"))
        .and_then(|v| v.as_object())
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();

    let dev_deps: Vec<String> = obj.and_then(|o| o.get("devDependencies"))
        .and_then(|v| v.as_object())
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();

    let pm = if project_path.join("bun.lock").exists() || project_path.join("bun.lockb").exists() { "bun" }
        else if project_path.join("pnpm-lock.yaml").exists() { "pnpm" }
        else if project_path.join("yarn.lock").exists() { "yarn" }
        else { "npm" };

    // Detect TS/Svelte/React
    let has_ts = deps.iter().chain(dev_deps.iter()).any(|d| d == "typescript");
    let has_svelte = deps.iter().chain(dev_deps.iter()).any(|d| d.contains("svelte"));
    let has_react = deps.iter().chain(dev_deps.iter()).any(|d| d == "react");
    let framework = if has_svelte { "svelte" }
        else if has_react { "react" }
        else if has_ts { "typescript" }
        else { "javascript" };

    json!({
        "name": name,
        "version": version,
        "package_manager": pm,
        "language": framework,
        "dependencies": deps,
        "dev_dependencies": dev_deps,
        "has_typescript": has_ts,
    })
}

fn parse_go_mod(content: &str, _project_path: &std::path::Path) -> Value {
    let mut module = String::new();
    let mut go_version = String::new();
    let mut deps = Vec::new();

    let mut in_require = false;
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("module ") {
            module = trimmed.strip_prefix("module ").unwrap_or("").to_string();
        } else if trimmed.starts_with("go ") {
            go_version = trimmed.strip_prefix("go ").unwrap_or("").to_string();
        } else if trimmed == "require (" {
            in_require = true;
        } else if trimmed == ")" {
            in_require = false;
        } else if in_require {
            if let Some(dep) = trimmed.split_whitespace().next() {
                if !dep.starts_with("//") {
                    deps.push(dep.to_string());
                }
            }
        }
    }

    json!({
        "name": module,
        "go_version": go_version,
        "dependencies": deps,
    })
}

fn parse_composer_json(content: &str, _project_path: &std::path::Path) -> Value {
    let parsed: Value = serde_json::from_str(content).unwrap_or(json!({}));
    let obj = parsed.as_object();

    let name = obj.and_then(|o| o.get("name")).and_then(|v| v.as_str()).unwrap_or("").to_string();
    let php_version = obj.and_then(|o| o.get("require"))
        .and_then(|v| v.as_object())
        .and_then(|r| r.get("php"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let deps: Vec<String> = obj.and_then(|o| o.get("require"))
        .and_then(|v| v.as_object())
        .map(|d| d.keys().filter(|k| *k != "php").cloned().collect())
        .unwrap_or_default();

    let dev_deps: Vec<String> = obj.and_then(|o| o.get("require-dev"))
        .and_then(|v| v.as_object())
        .map(|d| d.keys().cloned().collect())
        .unwrap_or_default();

    json!({
        "name": name,
        "php_version": php_version,
        "dependencies": deps,
        "dev_dependencies": dev_deps,
    })
}

fn count_project_files(path: &std::path::Path, extensions: &[&str]) -> (usize, usize) {
    let mut source = 0usize;
    let mut test = 0usize;
    count_files_recursive(path, extensions, &mut source, &mut test);
    (source, test)
}

fn count_files_recursive(dir: &std::path::Path, extensions: &[&str], source: &mut usize, test: &mut usize) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_str().unwrap_or("");

        // Skip hidden dirs, node_modules, vendor, target, __pycache__, .venv
        if name_str.starts_with('.') || name_str == "node_modules" || name_str == "vendor"
            || name_str == "target" || name_str == "__pycache__" || name_str == ".venv"
            || name_str == "venv"
        {
            continue;
        }

        if path.is_dir() {
            count_files_recursive(&path, extensions, source, test);
        } else if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if !extensions.contains(&ext) { continue; }

            let path_str = path.to_string_lossy();
            if path_str.contains("test") || path_str.contains("_test.") || path_str.contains(".test.") {
                *test += 1;
            } else {
                *source += 1;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs::File;
    use std::io::Write;
    use tempfile::TempDir;

    #[test]
    fn test_get_value_type_name() {
        assert_eq!(get_value_type_name(&json!("hello")), "string");
        assert_eq!(get_value_type_name(&json!(42)), "number");
        assert_eq!(get_value_type_name(&json!(3.14)), "number");
        assert_eq!(get_value_type_name(&json!(true)), "boolean");
        assert_eq!(get_value_type_name(&json!(false)), "boolean");
        assert_eq!(get_value_type_name(&json!([])), "array");
        assert_eq!(get_value_type_name(&json!({})), "object");
        assert_eq!(get_value_type_name(&json!(null)), "null");
    }

    #[test]
    fn test_parse_positive_int_valid() {
        let mut args = Map::new();
        args.insert("limit".to_string(), json!(10));

        let result = parse_positive_int(&args, "limit", None, None).unwrap();
        assert_eq!(result, Some(10));
    }

    #[test]
    fn test_parse_positive_int_missing() {
        let args = Map::new();
        let result = parse_positive_int(&args, "limit", None, None).unwrap();
        assert_eq!(result, None);
    }

    #[test]
    fn test_parse_positive_int_wrong_type() {
        let mut args = Map::new();
        args.insert("limit".to_string(), json!("not_a_number"));

        let result = parse_positive_int(&args, "limit", None, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a number"));
        assert!(err.contains("but got: string"));
    }

    #[test]
    fn test_parse_positive_int_negative() {
        let mut args = Map::new();
        args.insert("limit".to_string(), json!(-5));

        let result = parse_positive_int(&args, "limit", None, None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be a positive integer"));
    }

    #[test]
    fn test_parse_positive_int_min_validation() {
        let mut args = Map::new();
        args.insert("limit".to_string(), json!(5));

        // Should fail with min=10
        let result = parse_positive_int(&args, "limit", Some(10), None);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be at least 10"));
        assert!(err.contains("got: 5"));

        // Should pass with min=5
        let result = parse_positive_int(&args, "limit", Some(5), None);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(5));
    }

    #[test]
    fn test_parse_positive_int_max_validation() {
        let mut args = Map::new();
        args.insert("limit".to_string(), json!(100));

        // Should fail with max=50
        let result = parse_positive_int(&args, "limit", None, Some(50));
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("must be at most 50"));
        assert!(err.contains("got: 100"));

        // Should pass with max=100
        let result = parse_positive_int(&args, "limit", None, Some(100));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(100));
    }

    #[test]
    fn test_parse_positive_int_range_validation() {
        let mut args = Map::new();
        args.insert("depth".to_string(), json!(3));

        // Should pass with range 1-5
        let result = parse_positive_int(&args, "depth", Some(1), Some(5));
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), Some(3));

        // Test below range
        args.insert("depth".to_string(), json!(0));
        let result = parse_positive_int(&args, "depth", Some(1), Some(5));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be at least 1"));

        // Test above range
        args.insert("depth".to_string(), json!(10));
        let result = parse_positive_int(&args, "depth", Some(1), Some(5));
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("must be at most 5"));
    }

    #[test]
    fn test_validate_file_exists_valid() {
        // Create a temporary file
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "test content").unwrap();

        // Should pass validation (file is within workspace)
        let result = validate_file_exists(file_path.to_str().unwrap(), temp_dir.path());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_file_exists_missing() {
        let workspace = std::path::Path::new("/tmp");
        let result = validate_file_exists("/tmp/nonexistent_file_12345.txt", workspace);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("File not found"));
        assert!(err.contains("Please check the path"));
    }

    #[test]
    fn test_validate_file_exists_directory() {
        // Create a temporary directory
        let temp_dir = TempDir::new().unwrap();
        let nested = temp_dir.path().join("subdir");
        std::fs::create_dir(&nested).unwrap();

        // Should fail because it's a directory, not a file
        let result = validate_file_exists(nested.to_str().unwrap(), temp_dir.path());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Path exists but is not a file"));
        assert!(err.contains("Expected a regular file"));
    }

    #[test]
    fn test_validate_file_exists_outside_workspace() {
        let temp_dir = TempDir::new().unwrap();
        let workspace = temp_dir.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        // Create a file outside the workspace
        let outside_file = temp_dir.path().join("secret.txt");
        let mut file = File::create(&outside_file).unwrap();
        writeln!(file, "sensitive data").unwrap();

        // Should fail — file is outside workspace
        let result = validate_file_exists(outside_file.to_str().unwrap(), &workspace);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Access denied"));
        assert!(err.contains("outside the workspace"));
    }

    #[test]
    fn test_split_location_single_line() {
        let result = split_location("file.go:57").unwrap();
        assert_eq!(result.0, "file.go");
        assert_eq!(result.1, 57);
        assert_eq!(result.2, 57);
    }

    #[test]
    fn test_split_location_range() {
        let result = split_location("file.go:57-130").unwrap();
        assert_eq!(result.0, "file.go");
        assert_eq!(result.1, 57);
        assert_eq!(result.2, 130);
    }

    #[test]
    fn test_split_location_invalid_format() {
        let result = split_location("file.go");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid location format"));
    }

    #[test]
    fn test_split_location_invalid_line_number() {
        let result = split_location("file.go:abc");
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Invalid line number"));
    }

    // Helper to create a test server (API calls will fail but validation runs first)
    fn test_server() -> Server {
        use crate::api_client::APIClient;
        use crate::lsp::LSPManager;
        let api = APIClient::new("http://localhost:0".to_string(), "test-key".to_string()).unwrap();
        let lsp = LSPManager::new("/tmp");
        Server::new(api, lsp)
    }

    #[test]
    fn test_batch_empty_operations_rejected() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!([]));
        let result = batch(&mut server, args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at least 1"));
    }

    #[test]
    fn test_batch_missing_operations_rejected() {
        let mut server = test_server();
        let args = Map::new();
        let result = batch(&mut server, args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("operations"));
    }

    #[test]
    fn test_batch_exceeds_max_operations() {
        let mut server = test_server();
        let ops: Vec<Value> = (0..21)
            .map(|i| json!({"tool": format!("tool_{}", i), "args": {}}))
            .collect();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!(ops));
        let result = batch(&mut server, args);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("at most 20"));
    }

    #[test]
    fn test_batch_recursive_call_blocked() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!([
            {"tool": "batch", "args": {"operations": [{"tool": "get_task", "args": {}}]}}
        ]));
        let result = batch(&mut server, args).unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0]["success"], false);
        assert!(results[0]["error"].as_str().unwrap().contains("Recursive"));
    }

    #[test]
    fn test_batch_unknown_tool_returns_error_result() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!([
            {"tool": "nonexistent_tool_xyz", "args": {}}
        ]));
        let result = batch(&mut server, args).unwrap();
        assert_eq!(result["total"], 1);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["index"], 0);
        assert_eq!(results[0]["tool"], "nonexistent_tool_xyz");
        assert_eq!(results[0]["success"], false);
        assert!(results[0]["error"].as_str().unwrap().contains("Unknown tool"));
    }

    #[test]
    fn test_batch_result_structure() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!([
            {"tool": "unknown_a", "args": {}},
            {"tool": "unknown_b", "args": {}}
        ]));
        let result = batch(&mut server, args).unwrap();
        assert_eq!(result["total"], 2);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["index"], 0);
        assert_eq!(results[0]["tool"], "unknown_a");
        assert_eq!(results[1]["index"], 1);
        assert_eq!(results[1]["tool"], "unknown_b");
    }

    #[test]
    fn test_batch_missing_tool_field() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert("operations".to_string(), json!([
            {"args": {}}
        ]));
        let result = batch(&mut server, args).unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["success"], false);
        assert!(results[0]["error"].as_str().unwrap().contains("'tool'"));
    }

    #[test]
    fn test_parse_timestamp_secs_rfc3339_basic() {
        // 2026-04-11T01:30:00Z = 1775871000
        assert_eq!(parse_timestamp_secs("2026-04-11T01:30:00Z"), 1775871000);
    }

    #[test]
    fn test_parse_timestamp_secs_leap_day() {
        // 2024-02-29T12:00:00Z = 1709208000
        assert_eq!(parse_timestamp_secs("2024-02-29T12:00:00Z"), 1709208000);
    }

    #[test]
    fn test_parse_timestamp_secs_year_boundary() {
        // 2026-12-31T23:59:59Z = 1798761599
        assert_eq!(parse_timestamp_secs("2026-12-31T23:59:59Z"), 1798761599);
        // 2027-01-01T00:00:00Z = 1798761600
        assert_eq!(parse_timestamp_secs("2027-01-01T00:00:00Z"), 1798761600);
    }

    #[test]
    fn test_parse_timestamp_secs_microseconds() {
        // Postgres json_agg format with microseconds — should parse, truncating fractional seconds
        assert_eq!(parse_timestamp_secs("2026-04-11T01:30:00.123456Z"), 1775871000);
    }

    #[test]
    fn test_parse_timestamp_secs_malformed() {
        assert_eq!(parse_timestamp_secs("not a date"), 0);
    }

    #[test]
    fn test_parse_timestamp_secs_empty() {
        assert_eq!(parse_timestamp_secs(""), 0);
    }

    // CULTRA-905: analyze_files (plural) bulk endpoint tests.

    fn write_go_file(dir: &TempDir, name: &str, contents: &str) -> std::path::PathBuf {
        let path = dir.path().join(name);
        let mut f = File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        path
    }

    #[test]
    fn test_analyze_files_returns_one_entry_per_input() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write_go_file(&dir, "a.go", "package main\nfunc main() {}\n");
        let p2 = write_go_file(&dir, "b.go", "package main\nfunc helper() {}\n");
        let p3 = write_go_file(&dir, "c.go", "package main\nimport \"sync\"\nvar _ sync.Mutex\n");

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("file_paths".to_string(), json!([
            p1.to_str().unwrap(),
            p2.to_str().unwrap(),
            p3.to_str().unwrap(),
        ]));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_files_tool(args, &workspace).unwrap();

        assert_eq!(result["total"], 3);
        assert_eq!(result["succeeded"], 3);
        assert_eq!(result["failed"], 0);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_analyze_files_preserves_input_order() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..5)
            .map(|i| write_go_file(&dir, &format!("f{}.go", i), "package main\n"))
            .collect();

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("security"));
        args.insert("file_paths".to_string(), json!(
            paths.iter().map(|p| p.to_str().unwrap()).collect::<Vec<_>>()
        ));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_files_tool(args, &workspace).unwrap();
        let results = result["results"].as_array().unwrap();
        for (i, entry) in results.iter().enumerate() {
            assert_eq!(
                entry["file_path"].as_str().unwrap(),
                paths[i].to_str().unwrap(),
                "result {} out of order", i
            );
        }
    }

    #[test]
    fn test_analyze_files_isolates_per_file_failure() {
        let dir = tempfile::tempdir().unwrap();
        let good = write_go_file(&dir, "good.go", "package main\nfunc main() {}\n");
        let bad = dir.path().join("nonexistent.go");

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("file_paths".to_string(), json!([
            good.to_str().unwrap(),
            bad.to_str().unwrap(),
        ]));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_files_tool(args, &workspace).unwrap();
        assert_eq!(result["total"], 2);
        assert_eq!(result["succeeded"], 1);
        assert_eq!(result["failed"], 1);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["success"], json!(true));
        assert_eq!(results[1]["success"], json!(false));
        assert!(results[1]["error"].is_string());
    }

    #[test]
    fn test_analyze_files_rejects_invalid_analyzer() {
        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("garbage"));
        args.insert("file_paths".to_string(), json!(["/tmp/x.go"]));
        let workspace = std::path::PathBuf::from("/");
        let err = analyze_files_tool(args, &workspace).unwrap_err();
        assert!(err.to_string().contains("Invalid analyzer"));
    }

    #[test]
    fn test_analyze_files_rejects_empty_array() {
        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("security"));
        args.insert("file_paths".to_string(), json!([]));
        let workspace = std::path::PathBuf::from("/");
        let err = analyze_files_tool(args, &workspace).unwrap_err();
        assert!(err.to_string().contains("at least one"));
    }

    #[test]
    fn test_analyze_files_rejects_non_string_entry() {
        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("security"));
        args.insert("file_paths".to_string(), json!(["/tmp/x.go", 42]));
        let workspace = std::path::PathBuf::from("/");
        let err = analyze_files_tool(args, &workspace).unwrap_err();
        assert!(err.to_string().contains("not a string"));
    }

    // ========================================================================
    // CULTRA-1066: complexity analyzer filter params
    // ========================================================================

    fn fake_function(name: &str, cyc: u32, cog: u32) -> crate::ast::FunctionComplexity {
        crate::ast::FunctionComplexity {
            name: name.to_string(),
            location: format!("/fake.go:1-10"),
            line_start: 1,
            line_end: 10,
            lines: 10,
            cyclomatic: cyc,
            cognitive: cog,
            receiver: None,
            rating: "moderate".to_string(),
        }
    }

    fn fake_analysis() -> ComplexityAnalysis {
        ComplexityAnalysis {
            file_path: "/fake.go".to_string(),
            language: "go".to_string(),
            // Sorted by cyclomatic descending (matches analyzer output convention)
            functions: vec![
                fake_function("very_complex", 25, 30),
                fake_function("complex", 12, 15),
                fake_function("moderate", 5, 6),
                fake_function("trivial", 1, 1),
            ],
            summary: crate::ast::ComplexitySummary {
                total_functions: 4,
                avg_cyclomatic: 10.75,
                max_cyclomatic: 25,
                avg_cognitive: 13.0,
                max_cognitive: 30,
                complex_functions: 2,
                very_complex_functions: 1,
                total_lines: 40,
                hotspot: Some("very_complex".to_string()),
            },
        }
    }

    #[test]
    fn test_apply_complexity_filters_no_op_when_inactive() {
        let analysis = fake_analysis();
        let filters = ComplexityFilters::default();
        assert!(!filters.is_active());
        let out = apply_complexity_filters(analysis, &filters);
        assert_eq!(out.functions.len(), 4, "default filters should not drop anything");
    }

    #[test]
    fn test_apply_complexity_filters_min_cyclomatic_drops_below_threshold() {
        let filters = ComplexityFilters { min_cyclomatic: Some(10), ..Default::default() };
        assert!(filters.is_active());
        let out = apply_complexity_filters(fake_analysis(), &filters);
        assert_eq!(out.functions.len(), 2, "expected 2 funcs >= CC 10");
        assert!(out.functions.iter().all(|f| f.cyclomatic >= 10));
    }

    #[test]
    fn test_apply_complexity_filters_min_cognitive_drops_below_threshold() {
        let filters = ComplexityFilters { min_cognitive: Some(15), ..Default::default() };
        let out = apply_complexity_filters(fake_analysis(), &filters);
        assert_eq!(out.functions.len(), 2, "expected 2 funcs >= cognitive 15");
        assert!(out.functions.iter().all(|f| f.cognitive >= 15));
    }

    #[test]
    fn test_apply_complexity_filters_top_n_truncates() {
        let filters = ComplexityFilters { top_n: Some(2), ..Default::default() };
        let out = apply_complexity_filters(fake_analysis(), &filters);
        assert_eq!(out.functions.len(), 2);
        // Pre-sorted by cyclomatic desc, so top 2 are very_complex + complex
        assert_eq!(out.functions[0].name, "very_complex");
        assert_eq!(out.functions[1].name, "complex");
    }

    #[test]
    fn test_apply_complexity_filters_combined_intersect() {
        // min_cyclomatic=5 keeps {very_complex, complex, moderate}; top_n=2 truncates to {very_complex, complex}
        let filters = ComplexityFilters {
            min_cyclomatic: Some(5),
            top_n: Some(2),
            ..Default::default()
        };
        let out = apply_complexity_filters(fake_analysis(), &filters);
        assert_eq!(out.functions.len(), 2);
        assert_eq!(out.functions[0].name, "very_complex");
        assert_eq!(out.functions[1].name, "complex");
    }

    #[test]
    fn test_apply_complexity_filters_preserves_summary() {
        // File-level summary aggregates should NOT change when functions are filtered
        // (otherwise avg_cyclomatic etc. become "avg of the top N" — meaningless).
        let filters = ComplexityFilters { top_n: Some(1), ..Default::default() };
        let out = apply_complexity_filters(fake_analysis(), &filters);
        assert_eq!(out.summary.total_functions, 4, "summary preserves unfiltered count");
        assert_eq!(out.summary.max_cyclomatic, 25);
        assert_eq!(out.functions.len(), 1);
    }

    #[test]
    fn test_complexity_filters_parse_from_args() {
        let mut args = Map::new();
        args.insert("min_cyclomatic".to_string(), json!(5));
        args.insert("top_n".to_string(), json!(10));
        let f = ComplexityFilters::parse_from_args(&args).unwrap();
        assert_eq!(f.min_cyclomatic, Some(5));
        assert_eq!(f.min_cognitive, None);
        assert_eq!(f.top_n, Some(10));
        assert!(f.is_active());
    }

    #[test]
    fn test_complexity_filters_parse_from_args_empty() {
        let args = Map::new();
        let f = ComplexityFilters::parse_from_args(&args).unwrap();
        assert!(!f.is_active());
    }

    #[test]
    fn test_complexity_filters_parse_top_n_rejects_zero() {
        let mut args = Map::new();
        args.insert("top_n".to_string(), json!(0));
        let err = ComplexityFilters::parse_from_args(&args).unwrap_err();
        assert!(err.to_string().contains("at least 1"), "got: {}", err.to_string());
    }

    #[test]
    fn test_analyze_file_complexity_top_n_truncates_in_response() {
        // End-to-end: analyze a Go file with multiple functions and verify
        // top_n=1 returns exactly one function in the JSON response.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "multi.go", concat!(
            "package main\n",
            "func a() {}\n",
            "func b(x int) int { if x > 0 { return 1 } else { return 0 } }\n",
            "func c(x int) int { if x > 0 { return 1 }; if x < 0 { return -1 }; return 0 }\n",
        ));

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("top_n".to_string(), json!(1));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_file_tool(args, &workspace).unwrap();
        let funcs = result["functions"].as_array().expect("functions array");
        assert_eq!(funcs.len(), 1, "top_n=1 should return exactly 1 function");
        // Summary aggregates preserved
        assert_eq!(result["summary"]["total_functions"].as_u64().unwrap(), 3);
    }

    #[test]
    fn test_analyze_file_complexity_min_cyclomatic_drops_simple() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "multi.go", concat!(
            "package main\n",
            "func a() {}\n",  // CC=1
            "func b(x int) int { if x > 0 { return 1 } else { return 0 } }\n", // CC=2
        ));

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("min_cyclomatic".to_string(), json!(2));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_file_tool(args, &workspace).unwrap();
        let funcs = result["functions"].as_array().unwrap();
        assert!(funcs.iter().all(|f| f["cyclomatic"].as_u64().unwrap() >= 2),
            "expected all functions to have CC>=2, got: {:?}",
            funcs.iter().map(|f| (f["name"].as_str(), f["cyclomatic"].as_u64())).collect::<Vec<_>>());
        // function 'a' (CC=1) should be filtered out, leaving 'b' (CC>=2)
        assert!(funcs.iter().any(|f| f["name"].as_str() == Some("b")));
        assert!(!funcs.iter().any(|f| f["name"].as_str() == Some("a")));
    }

    #[test]
    fn test_analyze_files_complexity_filters_apply_per_file() {
        let dir = tempfile::tempdir().unwrap();
        let p1 = write_go_file(&dir, "a.go", concat!(
            "package main\n",
            "func a1() {}\n",
            "func a2(x int) int { if x > 0 { return 1 } else { return 0 } }\n",
        ));
        let p2 = write_go_file(&dir, "b.go", concat!(
            "package main\n",
            "func b1() {}\n",
            "func b2(x int) int { if x > 0 { return 1 }; if x < 0 { return -1 }; return 0 }\n",
        ));

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("file_paths".to_string(), json!([p1.to_str().unwrap(), p2.to_str().unwrap()]));
        args.insert("top_n".to_string(), json!(1));

        let workspace = std::path::PathBuf::from("/");
        let result = analyze_files_tool(args, &workspace).unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results.len(), 2);
        // Each file should report exactly 1 function (top_n applied per-file)
        for entry in results {
            let funcs = entry["result"]["functions"].as_array().unwrap();
            assert_eq!(funcs.len(), 1,
                "expected per-file top_n=1; entry: {}", entry["file_path"]);
        }
    }

    #[test]
    fn test_analyze_file_filters_ignored_for_non_complexity_analyzer() {
        // Filters should be silently ignored when analyzer != complexity.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "x.go", "package main\nfunc main() {}\n");

        let mut args = Map::new();
        args.insert("analyzer".to_string(), json!("security"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("top_n".to_string(), json!(1));

        let workspace = std::path::PathBuf::from("/");
        // Should not error — filters are no-op for non-complexity analyzers
        let _result = analyze_file_tool(args, &workspace).expect("filters should be ignored, not errored");
    }

    // CULTRA-906: caller enrichment tests (LSP graceful degradation).
    // The full callgraph path requires a running language server, which we
    // don't spin up in unit tests. These tests cover the no-LSP path: the
    // enrichment helper must return symbols unchanged (no callers field) when
    // LSP queries fail, and parse_file_ast must not panic with with_callers=true.

    #[test]
    fn test_enrich_symbols_degrades_gracefully_without_lsp() {
        use crate::ast::parser::Parser;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sample.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedHelper() int {{").unwrap();
        writeln!(f, "    return 42").unwrap();
        writeln!(f, "}}").unwrap();
        writeln!(f, "func unexportedHelper() int {{").unwrap();
        writeln!(f, "    return 7").unwrap();
        writeln!(f, "}}").unwrap();
        drop(f);

        let parser = Parser::new();
        let file_context = parser.parse_file(path.to_str().unwrap()).unwrap();
        let server = test_server();

        let result = enrich_symbols_with_callers(&file_context, &server);
        let arr = result.as_array().expect("expected array");
        assert!(!arr.is_empty(), "expected at least one symbol");

        // No symbol should panic. The "callers" field, if present, must be
        // a (possibly empty) array. LSPManager may either fail (no field) or
        // return an empty references list (empty array) — both are degraded
        // states we accept.
        for entry in arr {
            let obj = entry.as_object().expect("symbol should serialize as object");
            assert!(obj.contains_key("name"), "symbol missing name");
            if let Some(callers) = obj.get("callers") {
                assert!(callers.is_array(), "callers must be an array, got {:?}", callers);
            }
        }
    }

    // CULTRA-908: analyze_changes tests. We use a real on-disk git repo
    // (init + commit) to exercise the actual git diff invocation rather
    // than mocking. Each test sets up its own tempdir → no shared state.

    fn init_git_repo(dir: &std::path::Path) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git command failed to spawn");
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run(&["init", "-q", "-b", "main"]);
        run(&["config", "user.email", "test@example.com"]);
        run(&["config", "user.name", "Test"]);
        run(&["config", "commit.gpgsign", "false"]);
    }

    fn commit_all(dir: &std::path::Path, msg: &str) {
        let run = |args: &[&str]| {
            let out = std::process::Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .expect("git command failed");
            assert!(out.status.success(), "git {:?} failed: {}", args, String::from_utf8_lossy(&out.stderr));
        };
        run(&["add", "-A"]);
        run(&["commit", "-q", "-m", msg]);
    }

    #[test]
    fn test_analyze_changes_filters_to_changed_files() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        std::fs::write(dir.path().join("safe.go"), "package main\nfunc Helper() {}\n").unwrap();
        commit_all(dir.path(), "init");

        std::fs::write(dir.path().join("changed.go"), "package main\nfunc Changed() {}\n").unwrap();
        commit_all(dir.path(), "add changed");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD~1"));
        args.insert("analyzer".to_string(), json!("complexity"));
        let result = analyze_changes_tool(args, dir.path()).expect("should not error");

        let total = result["total"].as_u64().unwrap();
        assert_eq!(total, 1, "expected 1 file analyzed, got {}: {:?}", total, result);
        let results = result["results"].as_array().unwrap();
        assert!(results[0]["file_path"].as_str().unwrap().ends_with("changed.go"));
    }

    #[test]
    fn test_analyze_changes_no_changes_returns_empty() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD"));
        args.insert("analyzer".to_string(), json!("security"));
        let result = analyze_changes_tool(args, dir.path()).expect("should not error");
        assert_eq!(result["total"], 0);
        assert_eq!(result["results"], json!([]));
    }

    #[test]
    fn test_analyze_changes_invalid_ref_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("nonexistent-ref"));
        args.insert("analyzer".to_string(), json!("security"));
        let err = analyze_changes_tool(args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("git diff failed"), "expected git error, got: {}", err);
    }

    #[test]
    fn test_analyze_changes_filters_by_analyzer_extension() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        std::fs::write(dir.path().join("styles.css"), ".x { color: red; }\n").unwrap();
        std::fs::write(dir.path().join("more.go"), "package main\nfunc M() {}\n").unwrap();
        commit_all(dir.path(), "add mixed");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD~1"));
        args.insert("analyzer".to_string(), json!("concurrency"));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        assert_eq!(result["total"], 1);
        let results = result["results"].as_array().unwrap();
        assert!(results[0]["file_path"].as_str().unwrap().ends_with("more.go"));
    }

    // CULTRA-956: include_working_tree default + descriptive empty messages.

    #[test]
    fn test_analyze_changes_includes_working_tree_by_default() {
        // Working-tree file is uncommitted; with the new default it must
        // be included in the diff. Pre-fix this returned 0 because the
        // tool ran `git diff <since> HEAD` which excluded uncommitted work.
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        // Touch a.go and add a new file IN THE WORKING TREE only.
        std::fs::write(dir.path().join("a.go"), "package main\nfunc Live() {}\n").unwrap();
        std::fs::write(dir.path().join("b.go"), "package main\nfunc B() {}\n").unwrap();
        // Note: NOT committed.

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD"));
        args.insert("analyzer".to_string(), json!("complexity"));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        let total = result["total"].as_u64().unwrap();
        assert!(total >= 1,
            "working-tree changes should be included by default, got total={}", total);
        assert_eq!(result["include_working_tree"], true);
    }

    #[test]
    fn test_analyze_changes_committed_only_mode_excludes_working_tree() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        // Working-tree only — NOT committed.
        std::fs::write(dir.path().join("a.go"), "package main\nfunc New() {}\n").unwrap();

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD"));
        args.insert("analyzer".to_string(), json!("complexity"));
        args.insert("include_working_tree".to_string(), json!(false));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        // include_working_tree=false → only committed diff between HEAD..HEAD = empty.
        assert_eq!(result["total"], 0);
        assert_eq!(result["include_working_tree"], false);
    }

    #[test]
    fn test_analyze_changes_no_changes_message_is_specific() {
        // CULTRA-956: empty result must say "no changes found between X
        // and the working tree" instead of the misleading
        // "language filter rejected files."
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD"));
        args.insert("analyzer".to_string(), json!("complexity"));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        assert_eq!(result["total"], 0);
        let msg = result["message"].as_str().unwrap();
        assert!(
            msg.contains("No changes found")
                || msg.contains("no changes found"),
            "empty diff should say 'no changes found', got: {}",
            msg
        );
        assert!(!msg.contains("language filter rejected"),
            "empty diff must NOT blame the language filter when there's no diff at all");
    }

    #[test]
    fn test_analyze_changes_language_filter_message_is_specific() {
        // The OTHER empty case: there ARE changes, but none match the
        // analyzer's language filter. The message should say so explicitly
        // and report the changed-file count.
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        // Add a non-Go file (CSS isn't in the complexity analyzer's
        // extension list, so it'll be filtered out).
        std::fs::write(dir.path().join("styles.css"), ".x{color:red}\n").unwrap();
        commit_all(dir.path(), "add css");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD~1"));
        args.insert("analyzer".to_string(), json!("complexity"));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        assert_eq!(result["total"], 0);
        let msg = result["message"].as_str().unwrap();
        assert!(msg.contains("language filter") || msg.contains("none matched"),
            "should blame the language filter, got: {}", msg);
        assert!(result["total_changed_files"].as_u64().unwrap() >= 1,
            "should report the count of files that were filtered out");
    }

    #[test]
    fn test_analyze_changes_skips_deleted_files() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        std::fs::write(dir.path().join("doomed.go"), "package main\n").unwrap();
        commit_all(dir.path(), "init");

        std::fs::remove_file(dir.path().join("doomed.go")).unwrap();
        commit_all(dir.path(), "delete");

        let mut args = Map::new();
        args.insert("since".to_string(), json!("HEAD~1"));
        args.insert("analyzer".to_string(), json!("complexity"));
        let result = analyze_changes_tool(args, dir.path()).unwrap();
        assert_eq!(result["total"], 0, "deleted files must not appear in analysis");
    }

    // CULTRA-909: diff_file_ast tests. Real git repo with two commits.

    #[test]
    fn test_diff_file_ast_added_and_removed_symbols() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let file = dir.path().join("api.go");
        std::fs::write(&file, "package main\nfunc Old() {}\nfunc Stable() {}\n").unwrap();
        commit_all(dir.path(), "init");

        std::fs::write(&file, "package main\nfunc New() {}\nfunc Stable() {}\n").unwrap();
        commit_all(dir.path(), "swap Old for New");

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("HEAD~1"));
        let result = diff_file_ast_tool(args, dir.path()).expect("should not error");

        let added: Vec<_> = result["added"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        let removed: Vec<_> = result["removed"].as_array().unwrap().iter().map(|s| s["name"].as_str().unwrap().to_string()).collect();
        assert!(added.contains(&"New".to_string()), "expected New in added, got {:?}", added);
        assert!(removed.contains(&"Old".to_string()), "expected Old in removed, got {:?}", removed);
        // Stable function appears in neither.
        assert!(!added.contains(&"Stable".to_string()));
        assert!(!removed.contains(&"Stable".to_string()));
    }

    #[test]
    fn test_diff_file_ast_signature_change_in_modified() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let file = dir.path().join("sig.go");
        std::fs::write(&file, "package main\nfunc DoWork(x int) {}\n").unwrap();
        commit_all(dir.path(), "init");

        std::fs::write(&file, "package main\nfunc DoWork(x int, y string) {}\n").unwrap();
        commit_all(dir.path(), "add y param");

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("HEAD~1"));
        let result = diff_file_ast_tool(args, dir.path()).unwrap();

        let modified = result["modified"].as_array().unwrap();
        assert_eq!(modified.len(), 1, "expected 1 modified, got {:?}", modified);
        let entry = &modified[0];
        assert_eq!(entry["name"], "DoWork");
        let base_params = entry["base"]["parameters"].as_array().unwrap();
        let head_params = entry["head"]["parameters"].as_array().unwrap();
        assert_eq!(base_params.len(), 1, "base should have 1 param");
        assert_eq!(head_params.len(), 2, "head should have 2 params");
        assert!(head_params.iter().any(|p| p["name"] == "y"), "head should include y param: {:?}", head_params);
    }

    #[test]
    fn test_diff_file_ast_identical_files_empty_diff() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());

        let file = dir.path().join("same.go");
        std::fs::write(&file, "package main\nfunc Same() {}\n").unwrap();
        commit_all(dir.path(), "init");
        // No changes, just another commit.
        std::fs::write(dir.path().join("other.txt"), "x").unwrap();
        commit_all(dir.path(), "noise");

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("HEAD~1"));
        let result = diff_file_ast_tool(args, dir.path()).unwrap();

        assert_eq!(result["added"], json!([]));
        assert_eq!(result["removed"], json!([]));
        assert_eq!(result["modified"], json!([]));
    }

    #[test]
    fn test_diff_file_ast_invalid_ref_returns_error() {
        let dir = tempfile::tempdir().unwrap();
        init_git_repo(dir.path());
        let file = dir.path().join("a.go");
        std::fs::write(&file, "package main\n").unwrap();
        commit_all(dir.path(), "init");

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("nonexistent-ref-xyz"));
        let err = diff_file_ast_tool(args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("git show"), "expected git error, got: {}", err);
    }

    #[test]
    fn test_diff_file_ast_nested_git_repo_walks_up_to_repo_root() {
        // CULTRA-954 reproduction: sandbox at outer dir, git repo nested
        // one level deep (sandbox/crate/.git), source file at
        // sandbox/crate/api.go. The pre-fix code ran `git show
        // HEAD:crate/api.go` from the sandbox (which has no .git) and
        // failed. The fix walks up from file_path to find .git, runs git
        // from there, and computes the rel_path against the resolved
        // repo root (so the path is `api.go`, not `crate/api.go`).
        let outer = tempfile::tempdir().unwrap();
        let crate_dir = outer.path().join("vux");
        std::fs::create_dir_all(&crate_dir).unwrap();
        init_git_repo(&crate_dir);

        let file = crate_dir.join("api.go");
        std::fs::write(&file, "package main\nfunc Old() {}\n").unwrap();
        commit_all(&crate_dir, "init");

        std::fs::write(&file, "package main\nfunc New() {}\n").unwrap();
        commit_all(&crate_dir, "swap");

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("HEAD~1"));
        // Sandbox is the OUTER dir; the git repo is nested inside.
        let result = diff_file_ast_tool(args, outer.path())
            .expect("nested git repo should be discovered via walk-up");

        let added: Vec<_> = result["added"].as_array().unwrap().iter()
            .map(|s| s["name"].as_str().unwrap().to_string()).collect();
        let removed: Vec<_> = result["removed"].as_array().unwrap().iter()
            .map(|s| s["name"].as_str().unwrap().to_string()).collect();
        assert!(added.contains(&"New".to_string()),
            "expected New in added (proves git ran from the right cwd), got: {:?}", added);
        assert!(removed.contains(&"Old".to_string()),
            "expected Old in removed, got: {:?}", removed);
    }

    #[test]
    fn test_diff_file_ast_no_git_repo_returns_clear_error() {
        // CULTRA-954: file inside the sandbox but outside any git repo →
        // skip the cryptic "git show failed" error in favor of a clear
        // "no .git found" message naming the sandbox root.
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("orphan.go");
        std::fs::write(&file, "package main\n").unwrap();

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(file.to_str().unwrap()));
        args.insert("base_ref".to_string(), json!("HEAD"));
        let err = diff_file_ast_tool(args, dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(".git") && msg.contains("sandbox"),
            "error should mention .git and sandbox boundary, got: {}", msg);
    }

    // CULTRA-907: find_dead_code tests. Same constraint as caller enrichment:
    // we don't spin up a real LSP server in unit tests, so we cover the
    // graceful-degradation contract (LSP failure → symbol skipped, not flagged
    // as dead) and validate response shape.

    #[test]
    fn test_find_dead_code_returns_shape_without_lsp() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dead.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedA() {{}}").unwrap();
        writeln!(f, "func ExportedB() {{}}").unwrap();
        writeln!(f, "func privateC() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));

        let result = find_dead_code_tool(args, &server).expect("should not error");
        let obj = result.as_object().unwrap();

        assert!(obj.contains_key("file_path"));
        assert!(obj.contains_key("checked_symbols"));
        assert!(obj.contains_key("dead_symbols"));
        assert!(obj.contains_key("lsp_failures"));
        assert!(obj.contains_key("caveats"));

        // 2 exported functions checked; private one skipped.
        let checked = obj["checked_symbols"].as_u64().unwrap();
        assert_eq!(checked, 2, "expected 2 checked exported symbols, got {}", checked);

        // Without LSP, every symbol either gets skipped (lsp_failures bumped)
        // or returns no references (would be flagged dead). Either is a valid
        // degraded outcome — assert dead_symbols is an array and lsp_failures
        // is a non-negative integer.
        assert!(obj["dead_symbols"].is_array());
        assert!(obj["lsp_failures"].is_u64());
    }

    #[test]
    fn test_find_dead_code_rejects_missing_file_path() {
        let server = test_server();
        let args = Map::new();
        let err = find_dead_code_tool(args, &server).unwrap_err();
        assert!(err.to_string().contains("file_path"));
    }

    #[test]
    fn test_find_dead_code_confidence_field_present() {
        // CULTRA-945: every entry in dead_symbols must carry a confidence
        // field ("high" | "low") and a total_references count so callers
        // can tell "proven dead" apart from "LSP didn't index yet".
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deadc.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedUnused() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_dead_code_tool(args, &server).unwrap();

        // Verify caveats mention CULTRA-945 confidence behaviour.
        let caveats = result["caveats"].as_array().unwrap();
        let joined: String = caveats.iter().filter_map(|v| v.as_str()).collect::<Vec<_>>().join(" ");
        assert!(joined.contains("CULTRA-945") && joined.contains("confidence"),
            "expected CULTRA-945 confidence caveat, got: {}", joined);

        // Every dead entry must have confidence + total_references.
        let dead = result["dead_symbols"].as_array().unwrap();
        for entry in dead {
            let obj = entry.as_object().unwrap();
            assert!(obj.contains_key("confidence"), "dead entry missing confidence: {:?}", obj);
            assert!(obj.contains_key("total_references"), "dead entry missing total_references: {:?}", obj);
            let c = obj["confidence"].as_str().unwrap();
            assert!(c == "high" || c == "low", "confidence must be 'high' or 'low', got: {}", c);
        }
    }

    // CULTRA-947: index-cold status surfacing tests.

    #[test]
    fn test_find_dead_code_reports_cold_index_status() {
        // Without a running LSP, all symbol references come back empty —
        // the cold-index signature. Must surface lsp_index_status='cold'
        // AND a top-level warning field (not just a caveat).
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cold.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedA() {{}}").unwrap();
        writeln!(f, "func ExportedB() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_dead_code_tool(args, &server).unwrap();

        assert_eq!(result["lsp_index_status"], "cold",
            "expected cold status when no LSP index, got {:?}", result["lsp_index_status"]);
        let warning = result.get("warning").and_then(|v| v.as_str())
            .expect("cold status must surface a top-level warning");
        assert!(warning.contains("cold"), "warning should mention cold: {}", warning);
        assert!(warning.contains("require_warm_index"),
            "warning should point at the require_warm_index knob: {}", warning);
    }

    #[test]
    fn test_find_dead_code_unknown_status_when_nothing_checked() {
        // A file with only private symbols has zero exported callables to
        // check → lsp_index_status='unknown' and no warning.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("private_only.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func private1() {{}}").unwrap();
        writeln!(f, "func private2() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_dead_code_tool(args, &server).unwrap();

        assert_eq!(result["checked_symbols"], 0);
        assert_eq!(result["lsp_index_status"], "unknown");
        assert!(result.get("warning").is_none(),
            "unknown status must not emit a warning, got {:?}", result.get("warning"));
    }

    #[test]
    fn test_find_dead_code_require_warm_index_errors_on_cold() {
        // Strict mode: cold index + require_warm_index=true → early error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cold_strict.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("require_warm_index".to_string(), json!(true));
        let err = find_dead_code_tool(args, &server).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cold"), "expected cold-index error, got: {}", msg);
        assert!(msg.contains("require_warm_index"),
            "error should reference require_warm_index, got: {}", msg);
    }

    #[test]
    fn test_find_dead_code_require_warm_index_false_still_returns_result() {
        // Explicit false should be equivalent to omitting the flag — still
        // returns best-effort result with the warning, not an error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("cold_false.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("require_warm_index".to_string(), json!(false));
        let result = find_dead_code_tool(args, &server).expect("should not error in lax mode");
        assert_eq!(result["lsp_index_status"], "cold");
        assert!(result.get("warning").is_some(), "should still surface warning");
    }

    #[test]
    fn test_find_dead_code_skips_private_symbols() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only_private.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func helper1() {{}}").unwrap();
        writeln!(f, "func helper2() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));

        let result = find_dead_code_tool(args, &server).unwrap();
        assert_eq!(result["checked_symbols"], 0);
        assert_eq!(result["dead_symbols"], json!([]));
    }

    // CULTRA-948: find_references tests. Same constraint as find_dead_code
    // tests: unit tests don't spin up a real LSP server, so we cover the
    // parameter-validation contract, response shape, the role classifier
    // directly, and the cold-index graceful-degradation path.

    #[test]
    fn test_find_references_missing_symbol_param() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!("/tmp/whatever.go"));
        let err = find_references_tool(args, &server).unwrap_err();
        assert!(err.to_string().contains("symbol"),
            "expected symbol param error, got: {}", err);
    }

    #[test]
    fn test_find_references_missing_file_path_param() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Foo"));
        let err = find_references_tool(args, &server).unwrap_err();
        assert!(err.to_string().contains("file_path"),
            "expected file_path param error, got: {}", err);
    }

    #[test]
    fn test_find_references_symbol_not_in_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refs_missing.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ActualFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("NonexistentFn"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let err = find_references_tool(args, &server).unwrap_err();
        assert!(err.to_string().contains("NonexistentFn"),
            "expected symbol-not-found error naming the symbol, got: {}", err);
    }

    #[test]
    fn test_find_references_returns_shape_without_lsp() {
        // No LSP running → empty refs → cold-index path. Must produce a
        // well-formed response with the expected top-level fields, a cold
        // status, and a top-level warning.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refs_shape.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func MyFunc() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("MyFunc"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_references_tool(args, &server).expect("should not error in lax mode");
        let obj = result.as_object().unwrap();

        assert_eq!(obj["symbol"], "MyFunc");
        assert!(obj.contains_key("checked_at"));
        assert!(obj.contains_key("results"));
        assert!(obj.contains_key("total_references"));
        assert!(obj.contains_key("lsp_index_status"));
        assert!(obj["results"].is_array());
        assert_eq!(obj["lsp_index_status"], "cold");
        assert!(obj.get("warning").is_some(),
            "cold status must surface a top-level warning");
        let warning = obj["warning"].as_str().unwrap();
        assert!(warning.contains("cold"), "warning should mention cold: {}", warning);
        assert!(warning.contains("require_warm_index"),
            "warning should point at the require_warm_index knob: {}", warning);
    }

    #[test]
    fn test_find_references_require_warm_index_errors_on_cold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refs_strict.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func Strict() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Strict"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("require_warm_index".to_string(), json!(true));
        let err = find_references_tool(args, &server).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("cold"), "expected cold-index error, got: {}", msg);
        assert!(msg.contains("require_warm_index"),
            "error should reference require_warm_index, got: {}", msg);
    }

    #[test]
    fn test_find_references_require_warm_index_false_still_returns_result() {
        // Explicit false == omitting the flag → best-effort result with warning.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refs_lax.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func Lax() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Lax"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("require_warm_index".to_string(), json!(false));
        let result = find_references_tool(args, &server)
            .expect("explicit false should not error");
        assert_eq!(result["lsp_index_status"], "cold");
        assert!(result.get("warning").is_some());
    }

    // Role classifier unit tests — pure function, no LSP/fs needed.

    #[test]
    fn test_classify_reference_role_definition_shortcircuits() {
        assert_eq!(
            classify_reference_role("func Foo() {}", 5, 3, true),
            "definition"
        );
    }

    #[test]
    fn test_classify_reference_role_call_paren() {
        // `    self.build(arg)` — `build` at col 9, len 5, followed by '('
        assert_eq!(
            classify_reference_role("    self.build(arg)", 9, 5, false),
            "call"
        );
    }

    #[test]
    fn test_classify_reference_role_type_use_struct_literal() {
        // `let x = MyStruct { a: 1 };` — `MyStruct` at col 8, len 8, followed by ' {'
        assert_eq!(
            classify_reference_role("let x = MyStruct { a: 1 };", 8, 8, false),
            "type_use"
        );
    }

    #[test]
    fn test_classify_reference_role_type_use_path() {
        // `MyModule::new()` — `MyModule` at col 0, len 8, followed by '::'
        assert_eq!(
            classify_reference_role("MyModule::new()", 0, 8, false),
            "type_use"
        );
    }

    #[test]
    fn test_classify_reference_role_type_use_generic_open() {
        // `Vec<MyType>` — `Vec` at col 0, len 3, followed by '<'
        assert_eq!(
            classify_reference_role("Vec<MyType>", 0, 3, false),
            "type_use"
        );
    }

    #[test]
    fn test_classify_reference_role_doc_triple_slash() {
        // `/// See compose_inputs for details.` — symbol inside /// doc
        assert_eq!(
            classify_reference_role("/// See compose_inputs for details.", 8, 14, false),
            "doc"
        );
    }

    #[test]
    fn test_classify_reference_role_doc_line_comment() {
        // `// calls build(arg) internally` — symbol inside // line comment
        assert_eq!(
            classify_reference_role("// calls build(arg) internally", 9, 5, false),
            "doc"
        );
    }

    #[test]
    fn test_classify_reference_role_doc_trailing_comment() {
        // `x = 1; // compose_inputs is deprecated` — // opens before symbol col
        assert_eq!(
            classify_reference_role("x = 1; // compose_inputs is deprecated", 10, 14, false),
            "doc"
        );
    }

    #[test]
    fn test_classify_reference_role_doc_block_continuation() {
        // ` * uses compose_inputs ...` — multi-line /* */ block continuation
        assert_eq!(
            classify_reference_role(" * uses compose_inputs here", 8, 14, false),
            "doc"
        );
    }

    #[test]
    fn test_classify_reference_role_unknown_fallback() {
        // `foo;` — next char is ';', no match
        assert_eq!(
            classify_reference_role("foo;", 0, 3, false),
            "unknown"
        );
    }

    // CULTRA-950: warmup parameter wiring tests. Verify the find_dead_code
    // and find_references tools both surface a warmup_report in the
    // response when warmup=true, and omit it when warmup is unset.

    // CULTRA-938 / CULTRA-939: get_tasks + recent_activity tests for the
    // new /api/v2/tasks wrapped response shape and multi-status filtering.
    // These tests run against test_server() which has no live backend, so
    // the HTTP call always fails — what we're verifying is the validation
    // path BEFORE the HTTP call. A validation error proves the validator
    // ran; a network error means validation passed.

    fn err_is_status_validation(result: &Result<Value>) -> bool {
        match result {
            Err(e) => {
                let msg = e.to_string();
                msg.contains("Invalid status") || msg.contains("status array must")
                    || msg.contains("status must be")
            }
            Ok(_) => false,
        }
    }

    // CULTRA-958: end-to-end test for the concurrency analyzer's Rust path.
    // The internal analyze_concurrency_rust has 9 unit tests covering each
    // primitive (tokio::spawn, std::thread::spawn, Arc<Mutex>, mpsc, select!,
    // async/await, dedup), but those bypass the analyze_file dispatcher.
    // This test goes through the public entrypoint so the .rs → Rust
    // analyzer routing in run_analyzer is also under test.

    #[test]
    fn test_analyze_file_concurrency_rust_detects_tokio_primitives() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("daemon.rs");
        let body = r#"use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::sync::mpsc;
use tokio::select;

pub struct Daemon {
    state: Arc<Mutex<u32>>,
}

pub async fn run() {
    let (tx, mut rx) = mpsc::channel::<u32>(16);
    tokio::spawn(async move {
        tx.send(1).await.unwrap();
    });
    select! {
        _ = rx.recv() => {},
        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {},
    }
}
"#;
        std::fs::write(&path, body).unwrap();

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("analyzer".to_string(), json!("concurrency"));
        let result = analyze_file_tool(args, dir.path()).unwrap();

        assert_eq!(result["language"], "rust");

        let spawns = result["spawns"].as_array().expect("spawns should be array");
        assert!(!spawns.is_empty(),
            "tokio::spawn must be detected by the Rust analyzer dispatcher path, got: {:?}", spawns);

        let sync = result["synchronization"].as_array().expect("synchronization should be array");
        assert!(sync.iter().any(|s| s["kind"].as_str().unwrap_or("").contains("Mutex")),
            "Mutex must be detected, got: {:?}", sync);

        let channels = result["channels"].as_array().expect("channels should be array");
        assert!(!channels.is_empty(),
            "mpsc::channel must be detected, got: {:?}", channels);

        let selects = result["selects"].as_array().expect("selects should be array");
        assert!(!selects.is_empty(),
            "tokio::select! must be detected, got: {:?}", selects);

        let async_fns = result["async_functions"].as_array().expect("async_functions should be array");
        assert!(!async_fns.is_empty(),
            "async fn must be detected, got: {:?}", async_fns);

        let await_count = result["await_points"].as_u64().unwrap_or(0);
        assert!(await_count > 0,
            "await points must be detected, got: {}", await_count);
    }

    #[test]
    fn test_analyze_file_concurrency_rust_empty_for_sync_only_file() {
        // CULTRA-958: a Rust file with no concurrency primitives must
        // legitimately return all-empty fields. This is the case Tin-chan
        // saw on channel.rs and mistook for "the analyzer is Rust-blind."
        // Pinned here so future regressions stay diagnosable.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sync.rs");
        std::fs::write(&path, "pub fn add(a: i32, b: i32) -> i32 { a + b }\n").unwrap();

        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("analyzer".to_string(), json!("concurrency"));
        let result = analyze_file_tool(args, dir.path()).unwrap();

        assert_eq!(result["language"], "rust");
        assert_eq!(result["spawns"], json!([]));
        assert_eq!(result["synchronization"], json!([]));
        assert_eq!(result["channels"], json!([]));
        assert_eq!(result["selects"], json!([]));
        assert_eq!(result["async_functions"], json!([]));
        assert_eq!(result["await_points"], 0);
    }

    // CULTRA-959: resolve_project_id tests. The helper had been dead-code
    // since shipping; CULTRA-959 wires it into parse_file_ast and these
    // tests pin its behavior so it can't regress.

    #[test]
    fn test_resolve_project_id_passes_through_explicit_value() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("vestige"));
        resolve_project_id(&server, &mut args).unwrap();
        assert_eq!(args.get("project_id").unwrap(), "vestige");
    }

    #[test]
    fn test_resolve_project_id_falls_back_to_server_default() {
        // CULTRA-959: when project_id is missing from args, the helper
        // injects the harness-provided default. Previously parse_file_ast
        // hardcoded "proj-cultra" instead of consulting this default.
        use crate::api_client::APIClient;
        use crate::lsp::LSPManager;
        let api = APIClient::new("http://localhost:0".to_string(), "test-key".to_string()).unwrap();
        let lsp = LSPManager::new("/tmp");
        let server = Server::new(api, lsp).with_default_project(Some("vestige".to_string()));

        let mut args = Map::new();
        resolve_project_id(&server, &mut args).unwrap();
        assert_eq!(args.get("project_id").unwrap(), "vestige",
            "should inject the server's default_project_id");
    }

    #[test]
    fn test_resolve_project_id_errors_when_missing_and_no_default() {
        // No project_id in args AND no harness default → clear error,
        // not silent fallback to a wrong project.
        let server = test_server();
        let mut args = Map::new();
        let err = resolve_project_id(&server, &mut args).unwrap_err();
        assert!(err.to_string().contains("project_id"),
            "error should name the missing parameter, got: {}", err);
        assert!(err.to_string().contains("CLAUDE.md") || err.to_string().contains("default"),
            "error should explain where the default would come from, got: {}", err);
    }

    #[test]
    fn test_resolve_project_id_treats_empty_string_as_missing() {
        // Empty string falls through to the default (or errors).
        use crate::api_client::APIClient;
        use crate::lsp::LSPManager;
        let api = APIClient::new("http://localhost:0".to_string(), "test-key".to_string()).unwrap();
        let lsp = LSPManager::new("/tmp");
        let server = Server::new(api, lsp).with_default_project(Some("vestige".to_string()));

        let mut args = Map::new();
        args.insert("project_id".to_string(), json!(""));
        resolve_project_id(&server, &mut args).unwrap();
        assert_eq!(args.get("project_id").unwrap(), "vestige",
            "empty project_id should fall through to the harness default");
    }

    #[test]
    fn test_get_tasks_accepts_legacy_single_status_string() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("status".to_string(), json!("done"));
        let result = get_tasks(&server, args);
        assert!(!err_is_status_validation(&result),
            "single-string status should pass validation, got: {:?}", result);
    }

    #[test]
    fn test_get_tasks_accepts_multi_status_array() {
        // CULTRA-939: status as array of valid statuses → validation passes,
        // backend call fails (no live server), but the failure is NOT a
        // validation error.
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("status".to_string(), json!(["done", "cancelled"]));
        let result = get_tasks(&server, args);
        assert!(!err_is_status_validation(&result),
            "valid status array should pass validation, got: {:?}", result);
    }

    #[test]
    fn test_get_tasks_rejects_invalid_status_in_array() {
        // CULTRA-939: array containing a value not in TaskStatus → validation
        // fires before the HTTP call.
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("status".to_string(), json!(["done", "totally_made_up_xyzzy"]));
        let err = get_tasks(&server, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("totally_made_up_xyzzy") || msg.contains("Invalid status"),
            "expected validation error naming the bad value, got: {}", msg);
    }

    #[test]
    fn test_get_tasks_rejects_non_string_array_element() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("status".to_string(), json!(["done", 42]));
        let err = get_tasks(&server, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status array must contain only strings")
                || msg.contains("must be a string"),
            "expected element type error, got: {}", msg);
    }

    #[test]
    fn test_get_tasks_rejects_non_string_non_array_status() {
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-x"));
        args.insert("status".to_string(), json!(42));
        let err = get_tasks(&server, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("status must be a string or array"),
            "expected type error for scalar status, got: {}", msg);
    }

    #[test]
    fn test_recent_activity_unwraps_new_tasks_response_shape() {
        // CULTRA-938: document the shape coercion contract. The production
        // code uses this exact match expression to handle both the new
        // wrapped response {tasks, total, limit, offset} and the legacy bare
        // array. Pin both shapes here so a future refactor can't silently
        // break either branch.
        let new_shape = json!({"tasks": [{"task_id": "X-1"}, {"task_id": "X-2"}], "total": 2});
        let legacy_shape = json!([{"task_id": "X-1"}]);
        let empty_object = json!({});

        let unwrap = |v: Value| -> Value {
            match v.get("tasks").cloned() {
                Some(arr) => arr,
                None => v,
            }
        };

        let from_new = unwrap(new_shape);
        assert!(from_new.is_array(), "new wrapped shape must unwrap to array");
        assert_eq!(from_new.as_array().unwrap().len(), 2);

        let from_legacy = unwrap(legacy_shape);
        assert!(from_legacy.is_array(), "legacy bare array must pass through");
        assert_eq!(from_legacy.as_array().unwrap().len(), 1);

        let from_empty = unwrap(empty_object);
        // Empty object has no "tasks" key → falls through to original. The
        // downstream filter_recent will then call .as_array() → None →
        // empty vec, which is correct (no items match a missing list).
        assert!(from_empty.is_object());
    }

    #[test]
    fn test_find_dead_code_warmup_off_by_default_no_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nowarmup.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_dead_code_tool(args, &server).unwrap();
        assert!(result.get("warmup_report").is_none(),
            "warmup_report should be absent when warmup is not requested");
    }

    #[test]
    fn test_find_dead_code_warmup_on_surfaces_report() {
        // CULTRA-952: warmup now requires a manifest in the file's ancestor
        // chain. Create a go.mod alongside the .go file so resolve_warmup_target
        // succeeds and we exercise the run-warmup path (success or failure)
        // rather than the skipped-no-manifest path.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module testmod\n").unwrap();
        let path = dir.path().join("warmup.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("warmup".to_string(), json!(true));
        let result = find_dead_code_tool(args, &server).unwrap();
        let report = result.get("warmup_report")
            .and_then(|v| v.as_object())
            .expect("warmup_report should be present when warmup=true");

        assert_eq!(report["language"], "go");
        let status = report["status"].as_str().unwrap();
        assert!(
            matches!(status, "warm" | "failed" | "cached"),
            "status should be warm/failed/cached for go (got {})", status
        );
        assert!(report.contains_key("elapsed_ms"));
        assert!(report.contains_key("cached"));
        assert!(report.contains_key("manifest_dir"),
            "CULTRA-952: report must surface the resolved manifest_dir");
    }

    // CULTRA-952 (post-verification): the cargo-warm-but-LSP-cold race.
    // When warmup succeeds but the LSP query still returns cold, the
    // warning text must explicitly call out the race so the agent retries
    // instead of trusting the (correctly empty) result.

    #[test]
    fn test_build_cold_warning_calls_out_race_when_warmup_succeeded() {
        use crate::lsp::manager::WarmupReport;
        let report = WarmupReport {
            language: "rust".to_string(),
            status: "warm".to_string(),
            cached: false,
            elapsed_ms: 27000,
            command: Some("cargo check ...".to_string()),
            message: None,
            cached_status: None,
            manifest_dir: Some("/x".to_string()),
        };
        let warning = build_cold_warning_for_find_dead_code(5, Some(&report));
        assert!(warning.contains("despite warmup completing successfully"),
            "should mention the race: {}", warning);
        assert!(warning.contains("cargo-warm-but-LSP-cold")
                || warning.contains("Retry") || warning.contains("retry"),
            "should suggest retry: {}", warning);
        assert!(warning.contains("20-60s") || warning.contains("30s"),
            "should give the latency window: {}", warning);
    }

    #[test]
    fn test_build_cold_warning_calls_out_race_when_cached_warmup_was_warm() {
        // Cached warm result also counts — the original outcome was still
        // a successful cargo check, just replayed from cache.
        use crate::lsp::manager::WarmupReport;
        let report = WarmupReport {
            language: "rust".to_string(),
            status: "cached".to_string(),
            cached: true,
            elapsed_ms: 1234,
            command: Some("cargo check ...".to_string()),
            message: None,
            cached_status: Some("warm".to_string()),
            manifest_dir: Some("/x".to_string()),
        };
        let warning = build_cold_warning_for_find_dead_code(5, Some(&report));
        assert!(warning.contains("despite warmup"),
            "cached-warm should also trigger the race callout: {}", warning);
    }

    #[test]
    fn test_build_cold_warning_no_race_callout_when_warmup_failed() {
        // Failed warmup → standard cold-index warning, NOT the race message.
        // The agent shouldn't be told to "retry in 30s" when the actual
        // problem is that cargo check itself failed.
        use crate::lsp::manager::WarmupReport;
        let report = WarmupReport {
            language: "rust".to_string(),
            status: "failed".to_string(),
            cached: false,
            elapsed_ms: 100,
            command: Some("cargo check ...".to_string()),
            message: Some("cargo check failed".to_string()),
            cached_status: None,
            manifest_dir: Some("/x".to_string()),
        };
        let warning = build_cold_warning_for_find_dead_code(5, Some(&report));
        assert!(!warning.contains("despite warmup completing successfully"),
            "failed warmup must NOT trigger the race callout: {}", warning);
    }

    #[test]
    fn test_build_cold_warning_no_race_callout_when_warmup_not_requested() {
        // No warmup at all → standard cold-index warning that suggests
        // passing warmup=true.
        let warning = build_cold_warning_for_find_dead_code(5, None);
        assert!(!warning.contains("despite warmup"),
            "no-warmup case must not mention the race: {}", warning);
        assert!(warning.contains("warmup=true") || warning.contains("require_warm_index"),
            "no-warmup case should suggest the warmup or strict-mode opt-in: {}", warning);
    }

    #[test]
    fn test_build_cold_warning_for_find_references_calls_out_race() {
        use crate::lsp::manager::WarmupReport;
        let report = WarmupReport {
            language: "rust".to_string(),
            status: "warm".to_string(),
            cached: false,
            elapsed_ms: 27000,
            command: Some("cargo check ...".to_string()),
            message: None,
            cached_status: None,
            manifest_dir: Some("/x".to_string()),
        };
        let warning = build_cold_warning_for_find_references("compose_from_diff_screens", 1, Some(&report));
        assert!(warning.contains("compose_from_diff_screens"),
            "should name the symbol: {}", warning);
        assert!(warning.contains("despite warmup completing successfully"),
            "should call out the race: {}", warning);
    }

    #[test]
    fn test_find_dead_code_warmup_on_no_manifest_skipped() {
        // CULTRA-952: a .go file with no go.mod in its ancestor chain
        // returns status='skipped' with a message naming the missing manifest.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("orphan.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func ExportedFn() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("warmup".to_string(), json!(true));
        let result = find_dead_code_tool(args, &server).unwrap();
        let report = result.get("warmup_report")
            .and_then(|v| v.as_object())
            .expect("warmup_report should be present");
        assert_eq!(report["status"], "skipped");
        let msg = report["message"].as_str().unwrap();
        assert!(msg.contains("go.mod"),
            "skipped message should name the missing manifest, got: {}", msg);
    }

    #[test]
    fn test_find_dead_code_warmup_on_unsupported_language_skipped() {
        // Python has no warmup command → status='skipped', not an error.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("script.py");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "def exported_fn():").unwrap();
        writeln!(f, "    pass").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("warmup".to_string(), json!(true));
        let result = find_dead_code_tool(args, &server)
            .expect("warmup on python should not error — should skip gracefully");
        let report = result.get("warmup_report")
            .and_then(|v| v.as_object())
            .expect("warmup_report should be present");
        assert_eq!(report["status"], "skipped");
        assert!(report["message"].is_string(),
            "skipped report should explain why");
    }

    #[test]
    fn test_find_references_warmup_on_surfaces_report() {
        // CULTRA-952: same manifest requirement as find_dead_code.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("go.mod"), "module testmod\n").unwrap();
        let path = dir.path().join("refs_warm.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func Target() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Target"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("warmup".to_string(), json!(true));
        let result = find_references_tool(args, &server).unwrap();
        let report = result.get("warmup_report")
            .and_then(|v| v.as_object())
            .expect("warmup_report should be present when warmup=true");
        assert_eq!(report["language"], "go");
        assert!(report.contains_key("status"));
        assert!(report.contains_key("elapsed_ms"));
        assert!(report.contains_key("manifest_dir"),
            "CULTRA-952: report must surface the resolved manifest_dir");
    }

    #[test]
    fn test_find_references_warmup_off_by_default_no_report() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("refs_nowarm.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func Target() {{}}").unwrap();
        drop(f);

        let server = test_server();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Target"));
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let result = find_references_tool(args, &server).unwrap();
        assert!(result.get("warmup_report").is_none());
    }

    // CULTRA-949: analyze_symbol tests. Exercises the full analyze_complexity
    // pipeline against a synthesized Go file, so these do real tree-sitter
    // parses — same pattern the existing complexity integration tests use.
    // Reuses the write_go_file helper defined earlier in this test module.

    #[test]
    fn test_analyze_symbol_requires_file_path() {
        let dir = tempfile::tempdir().unwrap();
        let mut args = Map::new();
        args.insert("symbol".to_string(), json!("Foo"));
        let err = analyze_symbol_tool(args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("file_path"),
            "expected file_path error, got: {}", err);
    }

    #[test]
    fn test_analyze_symbol_requires_symbol() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "a.go",
            "package main\nfunc Foo() {}\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        let err = analyze_symbol_tool(args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("symbol"),
            "expected symbol error, got: {}", err);
    }

    #[test]
    fn test_analyze_symbol_rejects_unsupported_analyzer() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "a.go",
            "package main\nfunc Foo() {}\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Foo"));
        args.insert("analyzer".to_string(), json!("security"));
        let err = analyze_symbol_tool(args, dir.path()).unwrap_err();
        assert!(err.to_string().contains("complexity"),
            "expected analyzer rejection, got: {}", err);
    }

    #[test]
    fn test_analyze_symbol_not_found_lists_available() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "avail.go",
            "package main\nfunc Alpha() {}\nfunc Beta() {}\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Gamma"));
        let err = analyze_symbol_tool(args, dir.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("Gamma"), "should name the missing symbol: {}", msg);
        assert!(msg.contains("Alpha") || msg.contains("Beta"),
            "should list available functions: {}", msg);
    }

    #[test]
    fn test_analyze_symbol_returns_scalar_metrics_without_baseline() {
        // Simple function → cyclomatic/cognitive are scalars, delta_mode=false.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "scalar.go",
            "package main\nfunc Simple() int { return 1 }\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Simple"));
        let result = analyze_symbol_tool(args, dir.path()).unwrap();

        assert_eq!(result["name"], "Simple");
        assert_eq!(result["delta_mode"], false);
        assert!(result["cyclomatic"].is_number(),
            "cyclomatic should be scalar in non-delta mode, got: {:?}", result["cyclomatic"]);
        assert!(result["cognitive"].is_number());
        assert!(result["lines"].is_number());
        assert!(result["rating"].is_string());
        assert!(result.get("location").is_some());
        assert_eq!(result["analyzer"], "complexity");
    }

    #[test]
    fn test_analyze_symbol_delta_mode_renders_prev_now_delta() {
        // Inline baseline → each baseline-covered metric renders as
        // {prev, now, delta}.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "delta.go",
            "package main\nfunc Target() int { return 1 }\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Target"));
        args.insert("delta_against".to_string(), json!({
            "cyclomatic": 17,
            "cognitive": 26,
            "lines": 128,
            "rating": "complex"
        }));
        let result = analyze_symbol_tool(args, dir.path()).unwrap();

        assert_eq!(result["delta_mode"], true);

        let cyc = result["cyclomatic"].as_object().expect("cyclomatic should be object in delta mode");
        assert_eq!(cyc["prev"], 17);
        assert!(cyc.contains_key("now"));
        assert!(cyc.contains_key("delta"));
        let delta = cyc["delta"].as_i64().unwrap();
        let now = cyc["now"].as_i64().unwrap();
        assert_eq!(delta, now - 17);

        let rating = result["rating"].as_object().expect("rating should be object when baseline had rating");
        assert_eq!(rating["prev"], "complex");
        assert!(rating.contains_key("now"));
    }

    #[test]
    fn test_analyze_symbol_delta_mode_partial_baseline() {
        // Baseline with only cyclomatic → cyclomatic renders as delta,
        // other metrics stay scalar.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "partial.go",
            "package main\nfunc Partial() int { return 1 }\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Partial"));
        args.insert("delta_against".to_string(), json!({"cyclomatic": 10}));
        let result = analyze_symbol_tool(args, dir.path()).unwrap();

        assert_eq!(result["delta_mode"], true);
        assert!(result["cyclomatic"].is_object(),
            "cyclomatic should be delta-shaped when in baseline");
        assert!(result["cognitive"].is_number(),
            "cognitive should stay scalar when not in baseline");
        assert!(result["lines"].is_number());
        assert!(result["rating"].is_string());
    }

    #[test]
    fn test_analyze_symbol_branching_increases_cyclomatic() {
        // Sanity: a function with multiple branches has cyclomatic > 1.
        // Pinning a specific number would couple us to the analyzer's
        // counting, which is a test-the-implementation anti-pattern.
        let dir = tempfile::tempdir().unwrap();
        let path = write_go_file(&dir, "branchy.go",
            "package main\n\
             func Branchy(x int) int {\n\
             \tif x > 0 {\n\
             \t\tif x > 10 { return 2 }\n\
             \t\treturn 1\n\
             \t}\n\
             \tfor i := 0; i < 3; i++ { x++ }\n\
             \treturn x\n\
             }\n");
        let mut args = Map::new();
        args.insert("file_path".to_string(), json!(path.to_str().unwrap()));
        args.insert("symbol".to_string(), json!("Branchy"));
        let result = analyze_symbol_tool(args, dir.path()).unwrap();
        let cyc = result["cyclomatic"].as_u64().unwrap();
        assert!(cyc > 1, "branchy function should have cyclomatic > 1, got {}", cyc);
    }

    #[test]
    fn test_enrich_symbols_skips_unexported_symbols() {
        // Even if LSP were available, the helper must skip private symbols
        // entirely as a perf optimization. With no LSP, both branches yield
        // a symbol without callers — but importantly, no panic / no error.
        use crate::ast::parser::Parser;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("only_private.go");
        let mut f = File::create(&path).unwrap();
        writeln!(f, "package main").unwrap();
        writeln!(f, "func privateOne() {{}}").unwrap();
        writeln!(f, "func privateTwo() {{}}").unwrap();
        drop(f);

        let parser = Parser::new();
        let file_context = parser.parse_file(path.to_str().unwrap()).unwrap();
        let server = test_server();

        let result = enrich_symbols_with_callers(&file_context, &server);
        let arr = result.as_array().expect("expected array");
        for entry in arr {
            let obj = entry.as_object().unwrap();
            assert!(!obj.contains_key("callers"));
        }
    }

    // CULTRA-1050: get_plan(detail='ascii') render-knob passthrough
    // (build_plan_render_query is a pure function — testing it without
    // spinning up the API client mirrors the execution_waves.rs pattern).

    #[test]
    fn test_build_plan_render_query_empty_when_no_knobs() {
        let args = Map::new();
        let q = build_plan_render_query(&args, "status").unwrap();
        assert!(q.is_empty(), "no knobs + no detail=ascii should yield empty query, got {:?}", q);
    }

    #[test]
    fn test_build_plan_render_query_passes_through_when_ascii() {
        let mut args = Map::new();
        args.insert("width".to_string(), json!(120));
        args.insert("style".to_string(), json!("ascii"));
        args.insert("with_titles".to_string(), json!(true));
        let q = build_plan_render_query(&args, "ascii").unwrap();

        let lookup = |key: &str| -> String {
            q.iter().find(|(k, _)| k == key).map(|(_, v)| v.clone())
                .unwrap_or_else(|| panic!("expected {} in {:?}", key, q))
        };
        assert_eq!(lookup("width"), "120");
        assert_eq!(lookup("style"), "ascii");
        assert_eq!(lookup("with_titles"), "true");
    }

    #[test]
    fn test_build_plan_render_query_rejects_render_knobs_with_non_ascii_detail() {
        // Catches the agent typing detail='status' but supplying with_titles —
        // the knobs would have been silently dropped. Loud failure is better.
        let mut args = Map::new();
        args.insert("with_titles".to_string(), json!(true));
        let err = build_plan_render_query(&args, "status").unwrap_err();
        assert!(err.to_string().contains("only apply when detail='ascii'"),
            "expected detail-mismatch error, got: {}", err);
    }

    #[test]
    fn test_build_plan_render_query_rejects_zero_width() {
        let mut args = Map::new();
        args.insert("width".to_string(), json!(0));
        let err = build_plan_render_query(&args, "ascii").unwrap_err();
        assert!(err.to_string().contains("width"),
            "expected width error, got: {}", err);
    }

    #[test]
    fn test_build_plan_render_query_rejects_unknown_style() {
        let mut args = Map::new();
        args.insert("style".to_string(), json!("bold"));
        let err = build_plan_render_query(&args, "ascii").unwrap_err();
        assert!(err.to_string().contains("style"),
            "expected style error, got: {}", err);
    }

    #[test]
    fn test_build_plan_render_query_drops_with_titles_when_false() {
        let mut args = Map::new();
        args.insert("with_titles".to_string(), json!(false));
        let q = build_plan_render_query(&args, "ascii").unwrap();
        assert!(q.iter().all(|(k, _)| k != "with_titles"),
            "with_titles=false should be omitted, got {:?}", q);
    }

    // CULTRA-1069: compact_parallel passthrough on get_plan(detail='ascii').

    #[test]
    fn test_build_plan_render_query_passes_compact_parallel_when_true() {
        let mut args = Map::new();
        args.insert("compact_parallel".to_string(), json!(true));
        let q = build_plan_render_query(&args, "ascii").unwrap();
        let cp = q.iter().find(|(k, _)| k == "compact_parallel")
            .expect("compact_parallel=true should pass through");
        assert_eq!(cp.1, "true");
    }

    #[test]
    fn test_build_plan_render_query_drops_compact_parallel_when_false() {
        let mut args = Map::new();
        args.insert("compact_parallel".to_string(), json!(false));
        let q = build_plan_render_query(&args, "ascii").unwrap();
        assert!(q.iter().all(|(k, _)| k != "compact_parallel"),
            "compact_parallel=false should be omitted (default), got {:?}", q);
    }

    #[test]
    fn test_build_plan_render_query_rejects_compact_parallel_with_non_ascii_detail() {
        // compact_parallel only matters when detail='ascii'. Loud failure
        // beats silent drop, mirrors the gate on width/style/with_titles.
        let mut args = Map::new();
        args.insert("compact_parallel".to_string(), json!(true));
        let err = build_plan_render_query(&args, "status").unwrap_err();
        assert!(err.to_string().contains("compact_parallel") || err.to_string().contains("only apply when detail='ascii'"),
            "expected detail-mismatch error mentioning compact_parallel, got: {}", err);
    }

    // CULTRA-1057: get_project_estimate_accuracy shim. We can't test the HTTP
    // call without a live server; what we can test cheaply is the
    // missing-arg / invalid-arg validation paths the impl runs before the
    // network call.

    #[test]
    fn test_get_project_estimate_accuracy_rejects_missing_project_id() {
        let server = test_server();
        let err = get_project_estimate_accuracy(&server, Map::new()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("project_id"),
            "expected error naming project_id, got: {}", msg);
    }

    #[test]
    fn test_get_project_estimate_accuracy_rejects_invalid_project_id() {
        // validate_id rejects non-id-shaped strings (e.g., spaces). The shim
        // catches this before issuing the HTTP call so the agent sees a
        // clearer error than a 400 from the API.
        let server = test_server();
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("not a valid id"));
        let err = get_project_estimate_accuracy(&server, args).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("project_id"),
            "expected error naming project_id, got: {}", msg);
    }

    // CULTRA-1063: get_plan(group_by) shim validation. Mirrors the
    // build_plan_render_query test pattern.

    #[test]
    fn test_build_plan_render_query_passes_through_group_by_with_status() {
        let mut args = Map::new();
        args.insert("group_by".to_string(), json!("tag"));
        let q = build_plan_render_query(&args, "status").unwrap();
        let group_by = q.iter().find(|(k, _)| k == "group_by")
            .expect("group_by should pass through");
        assert_eq!(group_by.1, "tag");
    }

    #[test]
    fn test_build_plan_render_query_rejects_unknown_group_by() {
        let mut args = Map::new();
        args.insert("group_by".to_string(), json!("status"));
        let err = build_plan_render_query(&args, "status").unwrap_err();
        assert!(err.to_string().contains("group_by"),
            "expected error naming group_by, got: {}", err);
    }

    #[test]
    fn test_build_plan_render_query_rejects_group_by_with_non_status_detail() {
        // Catches the agent typing detail='ascii' but supplying group_by — the
        // grouping would be silently dropped at the backend. Loud failure is
        // better.
        let mut args = Map::new();
        args.insert("group_by".to_string(), json!("tag"));
        let err = build_plan_render_query(&args, "ascii").unwrap_err();
        assert!(err.to_string().contains("only applies when detail='status'"),
            "expected detail-mismatch error, got: {}", err);
    }

    // ========================================================================
    // contextual_search path resolution (CULTRA-1067)
    // ========================================================================

    fn write_file(path: &std::path::Path, content: &str) {
        let mut f = File::create(path).expect("create test file");
        f.write_all(content.as_bytes()).expect("write test file");
    }

    #[test]
    fn test_contextual_search_resolves_relative_path() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();
        let subdir = workspace.join("sub");
        std::fs::create_dir(&subdir).unwrap();
        write_file(&subdir.join("hello.txt"), "the quick brown fox\n");

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("fox"));
        args.insert("path".to_string(), json!("sub"));

        let result = contextual_search_tool(args, &workspace)
            .expect("relative path should resolve under workspace_root");
        let total = result.get("total").and_then(|v| v.as_u64()).unwrap();
        assert_eq!(total, 1, "expected 1 match for relative path 'sub'");
    }

    #[test]
    fn test_contextual_search_default_path_is_workspace_root() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();
        write_file(&workspace.join("hello.txt"), "the quick brown fox\n");

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("fox"));
        // no path arg

        let result = contextual_search_tool(args, &workspace)
            .expect("default path should be workspace_root");
        let total = result.get("total").and_then(|v| v.as_u64()).unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_contextual_search_accepts_absolute_inside_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();
        let subdir = workspace.join("sub");
        std::fs::create_dir(&subdir).unwrap();
        write_file(&subdir.join("hello.txt"), "the quick brown fox\n");

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("fox"));
        args.insert("path".to_string(), json!(subdir.to_string_lossy().to_string()));

        let result = contextual_search_tool(args, &workspace)
            .expect("absolute path inside workspace should work");
        let total = result.get("total").and_then(|v| v.as_u64()).unwrap();
        assert_eq!(total, 1);
    }

    #[test]
    fn test_contextual_search_rejects_traversal_via_relative() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("anything"));
        args.insert("path".to_string(), json!(".."));

        let err = contextual_search_tool(args, &workspace)
            .expect_err("relative `..` traversal must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must be within workspace"),
            "expected workspace-membership error, got: {}", msg
        );
    }

    #[test]
    fn test_contextual_search_rejects_absolute_outside_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("anything"));
        args.insert("path".to_string(), json!(outside.to_string_lossy().to_string()));

        let err = contextual_search_tool(args, &workspace)
            .expect_err("absolute path outside workspace must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must be within workspace"),
            "expected workspace-membership error, got: {}", msg
        );
    }

    #[test]
    fn test_contextual_search_rejects_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();

        let mut args = Map::new();
        args.insert("pattern".to_string(), json!("anything"));
        args.insert("path".to_string(), json!("does-not-exist"));

        let err = contextual_search_tool(args, &workspace)
            .expect_err("nonexistent path must error explicitly, not silently return zero hits");
        let msg = err.to_string();
        assert!(
            msg.contains("does not exist") || msg.contains("cannot be accessed"),
            "expected nonexistent-path error, got: {}", msg
        );
    }

    // ========================================================================
    // CULTRA-1070: project_info path resolution (sibling fix to CULTRA-1067)
    // ========================================================================

    #[test]
    fn test_project_info_resolves_relative_path() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();
        let subdir = workspace.join("sub");
        std::fs::create_dir(&subdir).unwrap();
        write_file(&subdir.join("Cargo.toml"), "[package]\nname=\"sub\"\nversion=\"0.1.0\"\n");

        let mut args = Map::new();
        args.insert("path".to_string(), json!("sub"));

        let result = project_info_tool_inner(args, &workspace)
            .expect("relative path should resolve under workspace_root");
        assert_eq!(result["language"], "rust");
    }

    #[test]
    fn test_project_info_rejects_traversal_via_relative() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();

        let mut args = Map::new();
        args.insert("path".to_string(), json!(".."));

        let err = project_info_tool_inner(args, &workspace)
            .expect_err("relative `..` traversal must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must be within workspace"),
            "expected workspace-membership error, got: {}", msg
        );
    }

    #[test]
    fn test_project_info_rejects_absolute_outside_workspace() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().join("workspace");
        std::fs::create_dir(&workspace).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir(&outside).unwrap();

        let mut args = Map::new();
        args.insert("path".to_string(), json!(outside.to_string_lossy().to_string()));

        let err = project_info_tool_inner(args, &workspace)
            .expect_err("absolute path outside workspace must be rejected");
        let msg = err.to_string();
        assert!(
            msg.contains("must be within workspace"),
            "expected workspace-membership error, got: {}", msg
        );
    }

    #[test]
    fn test_project_info_rejects_nonexistent_path() {
        let tmp = TempDir::new().unwrap();
        let workspace = tmp.path().to_path_buf();

        let mut args = Map::new();
        args.insert("path".to_string(), json!("does-not-exist"));

        let err = project_info_tool_inner(args, &workspace)
            .expect_err("nonexistent path must error explicitly");
        let msg = err.to_string();
        assert!(
            msg.contains("does not exist") || msg.contains("cannot be accessed"),
            "expected nonexistent-path error, got: {}", msg
        );
    }
}
