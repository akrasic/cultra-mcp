# LSP Integration Setup Guide

The Cultra MCP server includes Language Server Protocol (LSP) integration, providing semantic code intelligence for Go, Rust, TypeScript/JavaScript, and Python.

## Prerequisites

You need to install the LSP servers for the languages you want to use:

### Go (gopls)

```bash
go install golang.org/x/tools/gopls@latest
```

Verify installation:
```bash
gopls version
```

### Rust (rust-analyzer)

```bash
rustup component add rust-analyzer
```

Verify installation:
```bash
rust-analyzer --version
```

### TypeScript/JavaScript (typescript-language-server)

```bash
npm install -g typescript-language-server typescript
```

Verify installation:
```bash
typescript-language-server --version
```

### Python (pyright)

```bash
npm install -g pyright
```

Verify installation:
```bash
pyright --version
```

## Available LSP Tools

The MCP server exposes 5 LSP tools:

### 1. `lsp_document_symbols`

List all symbols (functions, classes, structs, etc.) in a document.

**Parameters:**
- `file_path` (required): Absolute path to the source file
- `workspace_root` (optional): Workspace root directory (defaults to current directory)

**Example:**
```json
{
  "file_path": "/path/to/file.go",
  "workspace_root": "/path/to/project"
}
```

**Returns:**
```json
{
  "count": 74,
  "format": "flat",
  "symbols": [
    {
      "name": "Server",
      "kind": "Struct",
      "location": {
        "uri": "file:///path/to/file.go",
        "range": {
          "start": {"line": 32, "character": 5},
          "end": {"line": 46, "character": 1}
        }
      }
    }
  ]
}
```

### 2. `lsp_find_references`

Find all references to a symbol at a given position.

**Parameters:**
- `file_path` (required): Absolute path to the source file
- `line` (required): Line number (0-indexed)
- `character` (required): Character offset (0-indexed)
- `workspace_root` (optional): Workspace root directory

**Example:**
```json
{
  "file_path": "/path/to/file.go",
  "line": 34,
  "character": 6,
  "workspace_root": "/path/to/project"
}
```

**Returns:**
```json
{
  "count": 15,
  "references": [
    {
      "uri": "file:///path/to/file.go",
      "range": {
        "start": {"line": 50, "character": 10},
        "end": {"line": 50, "character": 16}
      }
    }
  ]
}
```

### 3. `lsp_goto_definition`

Jump to the definition of a symbol at a given position.

**Parameters:**
- `file_path` (required): Absolute path to the source file
- `line` (required): Line number (0-indexed)
- `character` (required): Character offset (0-indexed)
- `workspace_root` (optional): Workspace root directory

**Example:**
```json
{
  "file_path": "/path/to/file.go",
  "line": 50,
  "character": 10
}
```

**Returns:**
```json
{
  "found": true,
  "location": {
    "uri": "file:///path/to/definition.go",
    "range": {
      "start": {"line": 34, "character": 5},
      "end": {"line": 34, "character": 11}
    }
  }
}
```

### 4. `lsp_hover`

Get type information and documentation for a symbol at a given position.

**Parameters:**
- `file_path` (required): Absolute path to the source file
- `line` (required): Line number (0-indexed)
- `character` (required): Character offset (0-indexed)
- `workspace_root` (optional): Workspace root directory

**Example:**
```json
{
  "file_path": "/path/to/file.go",
  "line": 34,
  "character": 6
}
```

**Returns:**
```json
{
  "found": true,
  "contents": "type Server struct { ... }",
  "range": {
    "start": {"line": 34, "character": 5},
    "end": {"line": 34, "character": 11}
  }
}
```

### 5. `lsp_workspace_symbols`

Search for symbols across the entire workspace.

**Parameters:**
- `query` (required): Symbol name to search for
- `language` (required): Programming language (e.g., "go", "rust", "typescript")
- `workspace_root` (optional): Workspace root directory

**Example:**
```json
{
  "query": "Server",
  "language": "go",
  "workspace_root": "/path/to/project"
}
```

**Returns:**
```json
{
  "count": 12,
  "symbols": [
    {
      "name": "Server",
      "kind": "Struct",
      "location": {
        "uri": "file:///path/to/file.go",
        "range": {
          "start": {"line": 32, "character": 5},
          "end": {"line": 46, "character": 1}
        }
      }
    }
  ]
}
```

## Testing via JSON-RPC

You can test LSP tools directly via JSON-RPC:

```bash
cat <<'EOF' | ./target/release/cultra-mcp
{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"test","version":"1.0"}}}
{"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":"lsp_document_symbols","arguments":{"file_path":"/path/to/file.go","workspace_root":"/path/to/project"}}}
EOF
```

## Running Integration Tests

Integration tests are available but require LSP servers to be installed:

```bash
# Run all integration tests
cargo test --test lsp_integration_test -- --ignored

# Run specific test
cargo test --test lsp_integration_test test_lsp_document_symbols_go -- --ignored --nocapture
```

## Troubleshooting

### LSP server not found

**Error:** `ServerStartFailed: gopls: No such file or directory`

**Solution:** Install the LSP server (see Prerequisites above) and ensure it's in your PATH:
```bash
which gopls  # or rust-analyzer, typescript-language-server, etc.
```

### Initialization timeout

Some LSP servers take longer to initialize, especially on first run or large projects.

**Solution:** Increase the timeout or ensure the workspace root is correct.

### No symbols found

**Error:** `No symbols found in document`

**Solution:**
- Ensure the file exists and is syntactically valid
- Check that the correct LSP server is installed for the file type
- Verify the workspace root contains the project's configuration files (go.mod, Cargo.toml, package.json, etc.)

### Invalid position errors

When using `lsp_hover`, `lsp_goto_definition`, or `lsp_find_references`, ensure:
- Line numbers are 0-indexed (first line is 0)
- Character offsets are 0-indexed and count UTF-16 code units
- The position points to an actual symbol (not whitespace or comments)

## Performance Considerations

- **First request:** LSP servers initialize on first use (adds latency)
- **Subsequent requests:** Servers are kept alive and reused (fast)
- **Concurrent languages:** Each language has its own server process
- **Memory:** LSP servers can use significant memory on large projects

## Logging

Enable debug logging to troubleshoot LSP issues:

```bash
RUST_LOG=debug ./target/release/cultra-mcp
```

You'll see log messages like:
```
DEBUG cultra_mcp::lsp::client: Starting LSP server: gopls ["serve"] (workspace: "/path/to/project")
INFO cultra_mcp::lsp::client: LSP server initialized successfully: go
DEBUG cultra_mcp::lsp::client: LSP request: textDocument/documentSymbol (id: 1)
```

## Supported Languages

| Language       | LSP Server                  | File Extensions        | Status |
|----------------|----------------------------|------------------------|--------|
| Go             | gopls                      | .go                    | ✅     |
| Rust           | rust-analyzer              | .rs                    | ✅     |
| TypeScript     | typescript-language-server | .ts, .tsx              | ✅     |
| JavaScript     | typescript-language-server | .js, .jsx              | ✅     |
| Python         | pyright                    | .py                    | 🚧     |

✅ = Fully tested
🚧 = Implemented but not extensively tested

## Architecture

```
┌─────────────────┐
│   MCP Server    │
│  (cultra-mcp)   │
└────────┬────────┘
         │
         ├─────────────┐
         │             │
    ┌────▼─────┐  ┌───▼──────┐
    │LSPManager│  │LSPClient │
    │(V2)      │  │          │
    └────┬─────┘  └───┬──────┘
         │            │
    ┌────▼────────────▼───┐
    │   LSP Servers       │
    │ ┌──────┐ ┌────────┐ │
    │ │gopls │ │rust-   │ │
    │ │      │ │analyzer│ │
    │ └──────┘ └────────┘ │
    └─────────────────────┘
```

- **LSPClient:** Manages communication with a single LSP server
- **LSPManagerV2:** Manages multiple LSP servers (one per language)
- **MCP Tools:** Expose LSP functionality via MCP protocol

## Future Enhancements

- [ ] Support for more LSP requests (code actions, formatting, etc.)
- [ ] Connection pooling and caching
- [ ] Workspace file watching
- [ ] Incremental document updates
- [ ] Multi-project support
- [ ] Python LSP support (pyright integration)

## Contributing

When adding support for a new language:

1. Add the language server binary to `get_server_command()` in `client.rs`
2. Add file extension mapping to `detect_language()` in `client.rs`
3. Add integration tests in `tests/lsp_integration_test.rs`
4. Update this documentation
5. Test with real code in that language

---

**Version:** 1.0 (Phase 3 complete)
**Last Updated:** February 2026
