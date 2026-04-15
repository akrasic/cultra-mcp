//! Shared test helpers for the `mcp` module tree. Test-only (`#[cfg(test)]`).
//!
//! Keeps fixtures that span multiple modules (e.g. `test_server`) in one place
//! so the per-module test suites don't duplicate setup. Module-specific
//! fixtures (e.g. `mk_monorepo` in project_map) stay in their own test modules.

#![cfg(test)]

use crate::mcp::server::Server;

/// Create a test server pointed at a non-routable API endpoint. Any HTTP call
/// will fail, but argument validation and other pre-HTTP logic runs normally.
/// Use this for shim tests that exercise validation without needing a live API.
pub(crate) fn test_server() -> Server {
    use crate::api_client::APIClient;
    use crate::lsp::LSPManager;
    let api = APIClient::new("http://localhost:0".to_string(), "test-key".to_string()).unwrap();
    let lsp = LSPManager::new("/tmp");
    Server::new(api, lsp)
}
