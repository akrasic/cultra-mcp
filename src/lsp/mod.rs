// LSP (Language Server Protocol) integration module
//
// This module provides Language Server Protocol client functionality,
// enabling semantic code analysis through language servers like gopls,
// rust-analyzer, tsserver, etc.
//
// Architecture:
// - `client`: LSP client that communicates with language servers via JSON-RPC
// - `types`: LSP protocol types and structures
// - `manager`: LSP server lifecycle management (Phase 2)
// - `tools`: MCP tool implementations for LSP functionality (Phase 3)

pub mod client;
pub mod manager;
pub mod tools;
pub mod types;

// Re-export main types for convenience
pub use manager::LSPManager;
