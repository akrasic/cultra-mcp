use super::protocol::Tool;
use super::types::{
    DecisionStatus, DocType, EnumValues, PlanStatus, Priority, SessionStrategy, TaskStatus,
    TaskType,
};
use super::Server;
use crate::ast::{
    analyze_concurrency, analyze_css, analyze_react_component, css_variable_graph, find_css_rules,
    find_interface_implementations, find_unused_selectors, resolve_tailwind_classes, Parser,
};
use crate::lsp::tools::{lsp_document_symbols, lsp_query, lsp_workspace_symbols};
use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use std::fmt;

// Embed templates at compile time
const CLAUDE_MD_TEMPLATE: &str = include_str!("../../CLAUDE.md.TEMPLATE");
const CLAUDE_TEMPLATE_GUIDE: &str = include_str!("../../CLAUDE_TEMPLATE_GUIDE.md");

/// Get all tool definitions
pub fn get_tool_definitions() -> Vec<Tool> {
    vec![
        Tool {
            name: "load_session_state".to_string(),
            description: "Load the most recent session state for a project to resume work. Supports multiple retrieval strategies based on time decay and access patterns. Optionally includes complete plan context with smart suggestions.".to_string(),
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
            description: "Query tasks with optional filters".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier"
                    },
                    "status": {
                        "type": "string",
                        "description": "Filter by status (todo, in_progress, blocked, done, cancelled)"
                    },
                    "priority": {
                        "type": "string",
                        "description": "Filter by priority (P0, P1, P2, P3)"
                    },
                    "assigned_to": {
                        "type": "string",
                        "description": "Filter by assignee"
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
            description: "Create or update a task. Task ID is auto-generated in PROJECT-NUMBER format (e.g., CULTRA-47). Do not provide task_id.".to_string(),
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
                        "description": "Task type (feature, bug, chore, research)"
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
            description: "Get complete dependency chain for a task showing all upstream (blocking) and downstream (blocked) tasks transitively. Useful for understanding task dependencies and planning work order.".to_string(),
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
                    }
                },
                "required": ["task_id"]
            }),
        },
        Tool {
            name: "update_task_status".to_string(),
            description: "Quickly update task status without requiring all parameters".to_string(),
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
                    }
                },
                "required": ["task_id", "status"]
            }),
        },
        Tool {
            name: "update_task".to_string(),
            description: "Update an existing task's content or metadata without requiring all fields".to_string(),
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
                        "description": "New type: feature, bug, chore, research (optional)"
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
                    }
                },
                "required": ["task_id"]
            }),
        },
        Tool {
            name: "task_dependency".to_string(),
            description: "Add or remove a dependency relationship between tasks".to_string(),
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
                        "description": "Markdown content (required)"
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
                    "doc_type": {
                        "type": "string",
                        "description": "New document type (optional)"
                    },
                    "tags": {
                        "type": "array",
                        "items": {"type": "string"},
                        "description": "New tags (optional)"
                    }
                },
                "required": ["document_id"]
            }),
        },
        Tool {
            name: "link_document".to_string(),
            description: "Link a document to one or more tasks".to_string(),
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
                        "description": "Array of task IDs to link (required)"
                    }
                },
                "required": ["document_id", "task_ids"]
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
            description: "Get plan overview or full details. Default (detail=\"status\"): tasks with dependencies, progress summary, next available tasks. detail=\"full\": Engine V3 content (problem, goal, approach), all tasks with progress logs, linked documents.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan_id": {
                        "type": "string",
                        "description": "Plan identifier (required)"
                    },
                    "detail": {
                        "type": "string",
                        "enum": ["status", "full"],
                        "description": "Detail level: 'status' (default) for overview, 'full' for Engine V3 details with progress logs and linked documents"
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
            description: "Parse a code file and extract AST metadata (symbols, functions, calls, imports). Returns semantic information about the code structure.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the code file to parse (.go, .ts, .tsx, .js, .jsx, .py, .rs, .php)"
                    },
                    "project_id": {
                        "type": "string",
                        "description": "Project identifier (optional, defaults to 'proj-cultra')"
                    }
                },
                "required": ["file_path"]
            }),
        },
        Tool {
            name: "analyze_file".to_string(),
            description: "Analyze a source file. analyzer=\"concurrency\": Go concurrency patterns (goroutines, channels, mutex, race conditions). analyzer=\"react\": React component structure (props, hooks, state, children). analyzer=\"css\": CSS structural metadata (selectors, specificity, variables, media queries). analyzer=\"css_variables\": CSS custom property dependency graph (var() chains, cycles, unresolved refs).".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "analyzer": {
                        "type": "string",
                        "enum": ["concurrency", "react", "css", "css_variables"],
                        "description": "Analyzer type (required)"
                    },
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the file to analyze"
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
            description: "LSP position query. action=\"references\": find all references. action=\"definition\": jump to definition. action=\"hover\": get type info and docs.".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "action": {
                        "type": "string",
                        "enum": ["references", "definition", "hover"],
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
                        "description": "Optional workspace root (defaults to current dir)"
                    }
                },
                "required": ["action", "file_path", "line", "character"]
            }),
        },
        Tool {
            name: "lsp_workspace_symbols".to_string(),
            description: "Search for symbols across the entire workspace using LSP".to_string(),
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
                        "description": "Optional workspace root (defaults to current dir)"
                    }
                },
                "required": ["query", "language"]
            }),
        },
        Tool {
            name: "lsp_document_symbols".to_string(),
            description: "List all symbols in a document using LSP".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file_path": {
                        "type": "string",
                        "description": "Absolute path to the source file"
                    },
                    "workspace_root": {
                        "type": "string",
                        "description": "Optional workspace root (defaults to current dir)"
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

/// Validate that a file path exists and is readable
fn validate_file_exists(file_path: &str) -> Result<()> {
    let path = std::path::Path::new(file_path);
    if !path.exists() {
        return Err(anyhow!(
            "File not found: '{}'. Please check the path and try again.",
            file_path
        ));
    }
    if !path.is_file() {
        return Err(anyhow!(
            "Path exists but is not a file: '{}'. Expected a regular file.",
            file_path
        ));
    }
    Ok(())
}

/// Validate that an ID parameter contains only safe characters
fn validate_id(field: &str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(anyhow!("{} cannot be empty", field));
    }
    if !value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
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
            let val = n
                .as_u64()
                .ok_or_else(|| anyhow!("{} must be a positive integer (got: {})", field, n))?;

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
        Some(Value::Null) => Ok(None), // Treat null as absent
        Some(val) => Err(anyhow!(
            "{} must be a string, but got: {}",
            field,
            get_value_type_name(val)
        )),
        None => Ok(None),
    }
}

// ========== Generic API Handlers ==========

/// Generic POST handler - creates or updates a resource
fn api_post(server: &Server, endpoint: &str, args: Map<String, Value>) -> Result<Value> {
    server.api.post(endpoint, Value::Object(args))
}

/// Generic PUT handler - updates a resource
fn api_put(server: &Server, endpoint: &str, args: Map<String, Value>) -> Result<Value> {
    server.api.put(endpoint, Value::Object(args))
}

/// Generic GET by ID handler
fn api_get_by_id(
    server: &Server,
    endpoint_template: &str, // e.g., "/api/tasks/{}"
    id_field: &str,          // e.g., "task_id"
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
            let joined: Vec<String> = arr
                .iter()
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
    endpoint_template: &str, // e.g., "/api/tasks/{}/dependencies/{}"
    id_fields: &[&str],      // e.g., ["task_id", "depends_on"]
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
pub fn call_tool(server: &mut Server, name: &str, args: Map<String, Value>) -> Result<Value> {
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
        // AST Tools
        "parse_file_ast" => parse_file_ast(server, args),
        "analyze_file" => analyze_file_tool(args),
        "find_interface_implementations" => find_interface_implementations_tool(args),
        // CSS Analysis Tools
        "find_css_rules" => find_css_rules_tool(args),
        "find_unused_selectors" => find_unused_selectors_tool(args),
        // CSS Analysis Tools V2
        "resolve_tailwind_classes" => resolve_tailwind_classes_tool(args),
        // LSP Tools
        "lsp" => lsp_query(args, &server.lsp),
        "lsp_workspace_symbols" => lsp_workspace_symbols(args, &server.lsp),
        "lsp_document_symbols" => lsp_document_symbols(args, &server.lsp),
        // Engine V2 Intelligence Tools
        "search_code_context" => search_code_context(server, args),
        "read_symbol_lines" => read_symbol_lines(args),
        "init_vector_db" => init_vector_db(server, args),
        "query_context" => query_context(server, args),
        "add_graph_edge" => add_graph_edge(server, args),
        "query_graph" => query_graph(server, args),
        "get_graph_neighbors" => get_graph_neighbors(server, args),
        // Batch execution
        "batch" => batch(server, args),
        // Built-in templates
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

/// Tool implementation: get_template — return built-in templates embedded at compile time
fn get_template(args: Map<String, Value>) -> Result<Value> {
    let name = args
        .get("name")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: name"))?;

    let (title, content) = match name {
        "claude_md" => ("CLAUDE.md Template", CLAUDE_MD_TEMPLATE),
        "template_guide" => ("CLAUDE.md Template Guide", CLAUDE_TEMPLATE_GUIDE),
        other => {
            return Err(anyhow!(
                "Invalid template '{}'. Must be 'claude_md' or 'template_guide'",
                other
            ))
        }
    };

    Ok(json!({
        "name": name,
        "title": title,
        "content": content
    }))
}

/// Tool implementation: load_session_state
fn load_session_state(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate and normalize strategy enum
    if let Some(strategy) = parse_enum_param::<SessionStrategy>(&args, "strategy")? {
        args.insert("strategy".to_string(), Value::String(strategy.to_string()));
    }

    api_get_with_filters(
        server,
        "/api/v2/sessions/latest",
        &["project_id"],
        &["strategy", "include_plan_context", "refresh_cache"],
        &args,
    )
}

/// Tool implementation: save_session_state
fn save_session_state(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Fix: Convert context_snapshot from string to object if needed
    if let Some(Value::String(json_str)) = args.get("context_snapshot") {
        // Parse JSON string into object
        match serde_json::from_str::<Value>(json_str) {
            Ok(obj) => {
                args.insert("context_snapshot".to_string(), obj);
            }
            Err(e) => {
                return Err(anyhow!("Invalid JSON in context_snapshot: {}", e));
            }
        }
    }

    // Fix: Convert working_memory from string to object if needed
    if let Some(Value::String(json_str)) = args.get("working_memory") {
        match serde_json::from_str::<Value>(json_str) {
            Ok(obj) => {
                args.insert("working_memory".to_string(), obj);
            }
            Err(e) => {
                return Err(anyhow!("Invalid JSON in working_memory: {}", e));
            }
        }
    }

    // Engine V3 validation for working_memory structure
    if let Some(wm) = args.get("working_memory") {
        if let Some(wm_obj) = wm.as_object() {
            // Check required fields
            if let Some(phase) = wm_obj.get("phase") {
                if !phase.is_string() || phase.as_str().unwrap_or("").trim().is_empty() {
                    return Err(anyhow!("working_memory.phase must be a non-empty string (e.g., 'Implementation', 'Testing', 'Planning')"));
                }
            } else {
                return Err(anyhow!("working_memory.phase is required (Engine V3)"));
            }

            if let Some(current_focus) = wm_obj.get("current_focus") {
                if !current_focus.is_string()
                    || current_focus.as_str().unwrap_or("").trim().is_empty()
                {
                    return Err(anyhow!("working_memory.current_focus must be a non-empty string describing what you're doing now"));
                }
            } else {
                return Err(anyhow!(
                    "working_memory.current_focus is required (Engine V3)"
                ));
            }

            if let Some(next_action) = wm_obj.get("next_action") {
                if !next_action.is_string() || next_action.as_str().unwrap_or("").trim().is_empty()
                {
                    return Err(anyhow!("working_memory.next_action must be a non-empty string with a specific next step"));
                }
            } else {
                return Err(anyhow!(
                    "working_memory.next_action is required (Engine V3)"
                ));
            }
        }
    }

    // Engine V3 validation for context_snapshot structure
    if let Some(cs) = args.get("context_snapshot") {
        if let Some(cs_obj) = cs.as_object() {
            // Check required field
            if let Some(next_session_start) = cs_obj.get("next_session_start") {
                if !next_session_start.is_string()
                    || next_session_start.as_str().unwrap_or("").trim().is_empty()
                {
                    return Err(anyhow!("context_snapshot.next_session_start must be a non-empty string with clear resuming instructions"));
                }
            } else {
                return Err(anyhow!(
                    "context_snapshot.next_session_start is required (Engine V3)"
                ));
            }
        }
    }

    api_post(server, "/api/v2/sessions", args)
}

/// Tool implementation: get_sessions
fn get_sessions(server: &Server, args: Map<String, Value>) -> Result<Value> {
    api_get_with_filters(
        server,
        "/api/v2/sessions",
        &["project_id"],
        &["limit"],
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
    if !args.contains_key("project_id") || args.get("project_id").and_then(|v| v.as_str()).is_none()
    {
        return Err(anyhow!("project_id is required"));
    }
    if !args.contains_key("name") || args.get("name").and_then(|v| v.as_str()).is_none() {
        return Err(anyhow!("name is required"));
    }
    api_post(server, "/api/v2/projects", args)
}

/// Tool implementation: get_tasks
fn get_tasks(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
    // Validate and normalize filter enums
    if let Some(status) = parse_enum_param::<TaskStatus>(&args, "status")? {
        args.insert("status".to_string(), Value::String(status.to_string()));
    }
    if let Some(priority) = parse_enum_param::<Priority>(&args, "priority")? {
        args.insert("priority".to_string(), Value::String(priority.to_string()));
    }

    api_get_with_filters(
        server,
        "/api/v2/tasks",
        &["project_id"],
        &["status", "priority", "assigned_to"],
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

    server
        .api
        .get(&format!("/api/v2/tasks/{}/chain", task_id), query)
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
            api_post(
                server,
                &format!("/api/v2/tasks/{}/dependencies", task_id),
                api_body,
            )
        }
        "remove" => api_delete(
            server,
            "/api/v2/tasks/{}/dependencies/{}",
            &["task_id", "depends_on"],
            &args,
        ),
        other => Err(anyhow!(
            "Invalid action '{}'. Must be 'add' or 'remove'",
            other
        )),
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
fn save_document(server: &Server, mut args: Map<String, Value>) -> Result<Value> {
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
        let tag_str: Vec<String> = tags
            .iter()
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

/// Tool implementation: link_document (consolidated from link_document + link_document_batch)
fn link_document(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let _document_id = args
        .get("document_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("document_id is required"))?;
    validate_id("document_id", _document_id)?;

    let task_ids = args
        .get("task_ids")
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow!("task_ids is required and must be an array"))?;
    if task_ids.is_empty() {
        return Err(anyhow!("task_ids must contain at least 1 task ID"));
    }
    for (i, task_id_val) in task_ids.iter().enumerate() {
        if let Some(task_id) = task_id_val.as_str() {
            validate_id(&format!("task_ids[{}]", i), task_id)?;
        }
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

    api_get_with_filters(server, "/api/v2/plans", &["project_id"], &["status"], &args)
}

/// Tool implementation: get_plan (consolidated from get_plan_status + get_plan_details)
fn get_plan(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let plan_id = args
        .get("plan_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: plan_id"))?;
    validate_id("plan_id", plan_id)?;

    let detail = args
        .get("detail")
        .and_then(|v| v.as_str())
        .unwrap_or("status");
    let endpoint = match detail {
        "status" => format!("/api/v2/plans/{}/status", plan_id),
        "full" => format!("/api/v2/plans/{}/details", plan_id),
        other => {
            return Err(anyhow!(
                "Invalid detail value '{}'. Must be 'status' or 'full'",
                other
            ))
        }
    };

    server.api.get(&endpoint, None)
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
fn parse_file_ast(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?;

    // Validate file exists and is readable
    validate_file_exists(file_path)?;

    // Get project_id (with default for backward compatibility)
    let project_id = args
        .get("project_id")
        .and_then(|v| v.as_str())
        .unwrap_or("proj-cultra");

    // Parse file locally (fast, no network overhead)
    let parser = Parser::new();
    let file_context = parser
        .parse_file(file_path)
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

    api_post(
        server,
        "/api/v2/ast/parse",
        body.as_object().unwrap().clone(),
    )?;

    // Return AST metadata
    Ok(json!({
        "success": true,
        "file_path": file_context.file_path,
        "language": file_context.language,
        "symbols": file_context.symbols,
        "imports": file_context.imports,
        "ast_stats": file_context.ast_stats
    }))
}

/// Tool implementation: analyze_file (consolidated from analyze_concurrency + analyze_react_component + analyze_css + css_variable_graph)
fn analyze_file_tool(args: Map<String, Value>) -> Result<Value> {
    let analyzer = args
        .get("analyzer")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: analyzer"))?;

    // Validate analyzer before doing any I/O
    match analyzer {
        "concurrency" | "react" | "css" | "css_variables" => {}
        other => {
            return Err(anyhow!(
                "Invalid analyzer '{}'. Must be 'concurrency', 'react', 'css', or 'css_variables'",
                other
            ))
        }
    }

    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing required parameter: file_path"))?;

    validate_file_exists(file_path)?;

    match analyzer {
        "concurrency" => {
            let analysis = analyze_concurrency(file_path)
                .map_err(|e| anyhow!("Failed to analyze concurrency: {}", e))?;
            serde_json::to_value(analysis).map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "react" => {
            let analysis = analyze_react_component(file_path)
                .map_err(|e| anyhow!("Failed to analyze React component: {}", e))?;
            serde_json::to_value(analysis).map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "css" => {
            let analysis =
                analyze_css(file_path).map_err(|e| anyhow!("Failed to analyze CSS: {}", e))?;
            serde_json::to_value(analysis).map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        "css_variables" => {
            let graph = css_variable_graph(file_path)
                .map_err(|e| anyhow!("Failed to build CSS variable graph: {}", e))?;
            serde_json::to_value(graph).map_err(|e| anyhow!("Failed to serialize result: {}", e))
        }
        _ => unreachable!("analyzer already validated"),
    }
}

/// Tool implementation: find_interface_implementations
fn find_interface_implementations_tool(args: Map<String, Value>) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?;

    // Validate file exists and is readable
    validate_file_exists(file_path)?;

    let interface_name = args.get("interface_name").and_then(|v| v.as_str());

    let analysis = find_interface_implementations(file_path, interface_name)
        .map_err(|e| anyhow!("Failed to find interface implementations: {}", e))?;

    serde_json::to_value(analysis).map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

// ========== CSS Analysis Tools ==========

/// Tool implementation: find_css_rules
fn find_css_rules_tool(args: Map<String, Value>) -> Result<Value> {
    let file_path = args
        .get("file_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing file_path"))?;

    let pattern = args
        .get("pattern")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing pattern"))?;

    validate_file_exists(file_path)?;

    let rules = find_css_rules(file_path, pattern)
        .map_err(|e| anyhow!("Failed to find CSS rules: {}", e))?;

    serde_json::to_value(rules).map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

/// Tool implementation: find_unused_selectors
fn find_unused_selectors_tool(args: Map<String, Value>) -> Result<Value> {
    let css_path = args
        .get("css_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing css_path"))?;

    let component_dir = args
        .get("component_dir")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("Missing component_dir"))?;

    validate_file_exists(css_path)?;

    // Walk component_dir for matching files
    let component_paths = collect_component_files(component_dir)?;
    let path_refs: Vec<&str> = component_paths.iter().map(|s| s.as_str()).collect();

    let unused = find_unused_selectors(css_path, &path_refs)
        .map_err(|e| anyhow!("Failed to find unused selectors: {}", e))?;

    serde_json::to_value(unused).map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

/// Walk a directory and collect .tsx/.ts/.jsx/.js/.html file paths
fn collect_component_files(dir: &str) -> Result<Vec<String>> {
    let mut files = Vec::new();
    let extensions = ["tsx", "ts", "jsx", "js", "html"];

    fn walk(path: &std::path::Path, extensions: &[&str], files: &mut Vec<String>) {
        if path.is_dir() {
            // Skip common non-source directories
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(
                    name,
                    "node_modules"
                        | ".git"
                        | "dist"
                        | "build"
                        | "target"
                        | ".next"
                        | "__pycache__"
                        | ".venv"
                        | "vendor"
                ) {
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
fn resolve_tailwind_classes_tool(args: Map<String, Value>) -> Result<Value> {
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
        validate_file_exists(path)?;
    }

    let class_refs: Vec<&str> = classes.iter().map(|s| s.as_str()).collect();
    let result = resolve_tailwind_classes(&class_refs, css_path)
        .map_err(|e| anyhow!("Failed to resolve Tailwind classes: {}", e))?;

    serde_json::to_value(result).map_err(|e| anyhow!("Failed to serialize result: {}", e))
}

// ========== Engine V2 Intelligence Tools ==========

/// Tool implementation: search_code_context
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

    // Extract filter parameters
    let symbol_name = args.get("symbol_name").and_then(|v| v.as_str());
    let symbol_type = args.get("symbol_type").and_then(|v| v.as_str());
    let file_pattern = args.get("file_pattern").and_then(|v| v.as_str());
    let calls = args.get("calls").and_then(|v| v.as_str());
    let receiver = args.get("receiver").and_then(|v| v.as_str());
    let scope = args.get("scope").and_then(|v| v.as_str());
    let limit = parse_positive_int(&args, "limit", Some(1), Some(500))?.unwrap_or(50) as usize;

    // Search through code context locally
    let mut results = Vec::new();

    if let Some(context_array) = code_context.as_array() {
        for ctx in context_array {
            if let Some(ctx_obj) = ctx.as_object() {
                // Filter by file_pattern if provided
                if let Some(pattern) = file_pattern {
                    if let Some(file_path) = ctx_obj.get("file_path").and_then(|v| v.as_str()) {
                        if !file_path.contains(pattern) {
                            continue;
                        }
                    }
                }

                // Search through symbols
                if let Some(symbols) = ctx_obj.get("symbols").and_then(|v| v.as_array()) {
                    for sym in symbols {
                        if let Some(sym_obj) = sym.as_object() {
                            let mut matches = true;

                            // Filter by symbol_name
                            if let Some(name_filter) = symbol_name {
                                if let Some(name) = sym_obj.get("name").and_then(|v| v.as_str()) {
                                    if !name.to_lowercase().contains(&name_filter.to_lowercase()) {
                                        matches = false;
                                    }
                                } else {
                                    matches = false;
                                }
                            }

                            // Filter by symbol_type
                            if matches {
                                if let Some(type_filter) = symbol_type {
                                    if let Some(sym_type) =
                                        sym_obj.get("type").and_then(|v| v.as_str())
                                    {
                                        if sym_type != type_filter {
                                            matches = false;
                                        }
                                    } else {
                                        matches = false;
                                    }
                                }
                            }

                            // Filter by receiver (Go only)
                            if matches {
                                if let Some(receiver_filter) = receiver {
                                    if let Some(sym_receiver) =
                                        sym_obj.get("receiver").and_then(|v| v.as_str())
                                    {
                                        if sym_receiver != receiver_filter {
                                            matches = false;
                                        }
                                    } else {
                                        matches = false;
                                    }
                                }
                            }

                            // Filter by scope
                            if matches {
                                if let Some(scope_filter) = scope {
                                    if let Some(sym_scope) =
                                        sym_obj.get("scope").and_then(|v| v.as_str())
                                    {
                                        if sym_scope != scope_filter {
                                            matches = false;
                                        }
                                    } else {
                                        matches = false;
                                    }
                                }
                            }

                            // Filter by calls
                            if matches {
                                if let Some(calls_filter) = calls {
                                    if let Some(sym_calls) =
                                        sym_obj.get("calls").and_then(|v| v.as_array())
                                    {
                                        let calls_match = sym_calls.iter().any(|c| {
                                            c.as_str()
                                                .map(|s| s.contains(calls_filter))
                                                .unwrap_or(false)
                                        });
                                        if !calls_match {
                                            matches = false;
                                        }
                                    } else {
                                        matches = false;
                                    }
                                }
                            }

                            if matches {
                                results.push(sym.clone());
                                if results.len() >= limit {
                                    break;
                                }
                            }
                        }
                    }
                }

                if results.len() >= limit {
                    break;
                }
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
fn read_symbol_lines(args: Map<String, Value>) -> Result<Value> {
    use std::fs::File;
    use std::io::{BufRead, BufReader};

    let (file_path, start_line, end_line) =
        if let Some(location) = args.get("location").and_then(|v| v.as_str()) {
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
                .ok_or_else(|| anyhow!("start_line is required when using file_path"))?
                as usize;

            let end_line = args
                .get("end_line")
                .and_then(|v| v.as_i64())
                .map(|v| v as usize)
                .unwrap_or(start_line);

            (file_path, start_line, end_line)
        };

    // Validate file exists and is readable
    validate_file_exists(&file_path)?;

    // Read file lines
    let file =
        File::open(&file_path).map_err(|e| anyhow!("Failed to open file {}: {}", file_path, e))?;

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

    api_post(
        server,
        "/api/v2/vector/query",
        body.as_object().unwrap().clone(),
    )
}

/// Tool implementation: add_graph_edge
fn add_graph_edge(server: &Server, args: Map<String, Value>) -> Result<Value> {
    let required_fields = [
        "from_type",
        "from_id",
        "to_type",
        "to_id",
        "edge_type",
        "project_id",
    ];
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
    let colon_pos = location.rfind(':').ok_or_else(|| {
        anyhow!(
            "Invalid location format '{}' - expected 'file:line' or 'file:start-end'",
            location
        )
    })?;
    let file_path = location[..colon_pos].to_string();
    let line_part = &location[colon_pos + 1..];

    if line_part.contains('-') {
        // Range format: "57-130"
        let range: Vec<&str> = line_part.splitn(2, '-').collect();
        if range.len() != 2 {
            return Err(anyhow!("Invalid line range format"));
        }

        let start_line = range[0]
            .parse::<usize>()
            .map_err(|_| anyhow!("Invalid start line number"))?;
        let end_line = range[1]
            .parse::<usize>()
            .map_err(|_| anyhow!("Invalid end line number"))?;

        Ok((file_path, start_line, end_line))
    } else {
        // Single line: "57"
        let line = line_part
            .parse::<usize>()
            .map_err(|_| anyhow!("Invalid line number"))?;
        Ok((file_path, line, line))
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be at least 1"));

        // Test above range
        args.insert("depth".to_string(), json!(10));
        let result = parse_positive_int(&args, "depth", Some(1), Some(5));
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("must be at most 5"));
    }

    #[test]
    fn test_validate_file_exists_valid() {
        // Create a temporary file
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        let mut file = File::create(&file_path).unwrap();
        writeln!(file, "test content").unwrap();

        // Should pass validation
        let result = validate_file_exists(file_path.to_str().unwrap());
        assert!(result.is_ok());
    }

    #[test]
    fn test_validate_file_exists_missing() {
        let result = validate_file_exists("/tmp/nonexistent_file_12345.txt");
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("File not found"));
        assert!(err.contains("Please check the path"));
    }

    #[test]
    fn test_validate_file_exists_directory() {
        // Create a temporary directory
        let temp_dir = TempDir::new().unwrap();

        // Should fail because it's a directory, not a file
        let result = validate_file_exists(temp_dir.path().to_str().unwrap());
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Path exists but is not a file"));
        assert!(err.contains("Expected a regular file"));
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
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid location format"));
    }

    #[test]
    fn test_split_location_invalid_line_number() {
        let result = split_location("file.go:abc");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("Invalid line number"));
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
        args.insert(
            "operations".to_string(),
            json!([
                {"tool": "batch", "args": {"operations": [{"tool": "get_task", "args": {}}]}}
            ]),
        );
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
        args.insert(
            "operations".to_string(),
            json!([
                {"tool": "nonexistent_tool_xyz", "args": {}}
            ]),
        );
        let result = batch(&mut server, args).unwrap();
        assert_eq!(result["total"], 1);
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["index"], 0);
        assert_eq!(results[0]["tool"], "nonexistent_tool_xyz");
        assert_eq!(results[0]["success"], false);
        assert!(results[0]["error"]
            .as_str()
            .unwrap()
            .contains("Unknown tool"));
    }

    #[test]
    fn test_batch_result_structure() {
        let mut server = test_server();
        let mut args = Map::new();
        args.insert(
            "operations".to_string(),
            json!([
                {"tool": "unknown_a", "args": {}},
                {"tool": "unknown_b", "args": {}}
            ]),
        );
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
        args.insert(
            "operations".to_string(),
            json!([
                {"args": {}}
            ]),
        );
        let result = batch(&mut server, args).unwrap();
        let results = result["results"].as_array().unwrap();
        assert_eq!(results[0]["success"], false);
        assert!(results[0]["error"].as_str().unwrap().contains("'tool'"));
    }
}
