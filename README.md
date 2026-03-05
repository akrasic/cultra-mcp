# Cultra MCP

Universal MCP server for cross-tool AI coding intelligence. Written in Rust, communicates over stdio using the [Model Context Protocol](https://modelcontextprotocol.io/).

Cultra MCP provides project management, code analysis, knowledge graph, and LSP integration tools to AI coding assistants.
Support Claude Code, tested with OpenCode. 

## Requirements

You need a Cultra account to use this MCP server. Sign up at https://app.cultra.dev/ to get your API key.

## Building

Requires Rust 1.70+.

```bash
cargo build --release
```

or use `cargo install`

```
  cargo install --git https://github.com/akrasic/cultra-mcp
```

The binary is output to `./target/release/cultra-mcp`.

## Configuration

The server reads its configuration from `~/.config/cultra/mcp.json`:

```json
{
  "api": {
    "base_url": "https://api.cultra.dev",
    "key": "your-api-key-from-app.cultra.dev"
  }
}
```

### Config resolution order

1. **`CULTRA_MCP_CONFIG` env var** - If set, reads the config file from this path
2. **`~/.config/cultra/mcp.json`** - Default config file location
3. **Environment variables fallback** - `CULTRA_API_URL` (defaults to `http://localhost:8080`) and `CULTRA_API_KEY`

## Usage

### Claude Code

```
claude mcp add cultra /path/to/binary  
```

Add to your Claude Code MCP settings (`~/.claude/settings.json`):

```json
{
  "mcpServers": {
    "cultra": {
      "command": "/path/to/cultra-mcp"
    }
  }
}
```

### Generic stdio

The server communicates over stdin/stdout using JSON-RPC. It auto-detects the transport framing (Content-Length headers or newline-delimited), or you can force it:

```bash
cultra-mcp --transport=framed   # Content-Length framing
cultra-mcp --transport=line     # Newline-delimited
cultra-mcp --transport=auto     # Auto-detect (default)
```

### Logging

Logs go to stderr. Control verbosity with `RUST_LOG`:

```bash
RUST_LOG=debug cultra-mcp
```

## Tools

### Session & Project Management

| Tool | Description |
|------|-------------|
| `load_session_state` | Load the most recent session state for a project to resume work |
| `save_session_state` | Save current session state for future continuity |
| `create_project` | Create a new project |
| `get_sessions` | List sessions for a project |
| `get_session` | Get a specific session by ID |

### Task Management

| Tool | Description |
|------|-------------|
| `get_tasks` | Query tasks with optional filters (status, priority, assignee) |
| `search_tasks` | Search tasks by text query |
| `save_task` | Create or update a task |
| `get_task` | Get a specific task by ID |
| `get_task_chain` | Get a task and its dependency chain |
| `update_task_status` | Update task status (todo, in_progress, blocked, done, cancelled) |
| `update_task` | Update task fields |
| `task_dependency` | Add or remove task dependencies |
| `add_progress_log` | Add a progress log entry to a task |

### Documents

| Tool | Description |
|------|-------------|
| `save_document` | Save a document (guide, architecture, test report, etc.) |
| `get_documents` | Query documents with optional filters |
| `get_document` | Get a specific document by ID |
| `update_document` | Update document fields |
| `link_document` | Link a document to a task or plan |

### Plans & Decisions

| Tool | Description |
|------|-------------|
| `save_plan` | Save a plan with milestones |
| `get_plans` | Query plans for a project |
| `get_plan` | Get a specific plan with details |
| `save_decision` | Record an architectural/design decision |
| `get_decisions` | Query decisions for a project |

### Code Analysis (AST)

Built-in tree-sitter based analysis for Go, TypeScript, JavaScript, Python, and Rust.

| Tool | Description |
|------|-------------|
| `parse_file_ast` | Parse a file and extract symbols (functions, types, etc.) |
| `analyze_file` | Analyze concurrency patterns, React components, etc. |
| `find_interface_implementations` | Find implementations of an interface/trait |
| `find_css_rules` | Find CSS rules matching selectors |
| `find_unused_selectors` | Detect unused CSS selectors |
| `resolve_tailwind_classes` | Resolve Tailwind utility classes |
| `search_code_context` | Search code with semantic context |
| `read_symbol_lines` | Read source lines for a specific symbol |

### Knowledge Graph

| Tool | Description |
|------|-------------|
| `add_graph_edge` | Add an edge between entities in the knowledge graph |
| `query_graph` | Query the knowledge graph |
| `get_graph_neighbors` | Get neighboring entities in the graph |

### LSP Integration

Semantic code intelligence via Language Server Protocol. Requires external LSP servers (see [docs/LSP_SETUP.md](docs/LSP_SETUP.md)).

| Tool | Description |
|------|-------------|
| `lsp` | Run LSP operations (hover, goto definition, find references) |
| `lsp_workspace_symbols` | Search for symbols across the workspace |
| `lsp_document_symbols` | List all symbols in a document |

### Utilities

| Tool | Description |
|------|-------------|
| `init_vector_db` | Initialize the vector database for semantic search |
| `query_context` | Query the context engine |
| `batch` | Execute multiple tool calls in a single request |
| `get_template` | Get a prompt/document template |

## Supported Languages

| Language | AST Parsing | LSP Support |
|----------|-------------|-------------|
| Go | Yes | Yes (gopls) |
| TypeScript | Yes | Yes (typescript-language-server) |
| JavaScript | Yes | Yes (typescript-language-server) |
| Python | Yes | Yes (pyright) |
| Rust | Yes | Yes (rust-analyzer) |

## License

See [LICENSE](LICENSE) for details.
