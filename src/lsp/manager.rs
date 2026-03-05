// LSP Server Manager
//
// Manages multiple LSP server lifecycles, keeping them alive across requests
// and providing lazy initialization for each language.

use super::client::{LSPClient, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

// ============================================================================
// LSP Manager
// ============================================================================

/// Manages multiple LSP server instances, one per language.
///
/// Stores clients as `Arc<Mutex<LSPClient>>` in a HashMap for thread-safe
/// sharing. Clients are lazily initialized on first request and persist
/// across multiple tool calls, avoiding the cost of spawning a new
/// language server process per request.
pub struct LSPManager {
    /// Active LSP clients, keyed by language.
    /// Each client is wrapped in Arc<Mutex<...>> for thread-safe sharing.
    clients: Mutex<HashMap<String, Arc<Mutex<LSPClient>>>>,
    /// Ad-hoc clients for non-default workspace roots, keyed by "language:workspace_root".
    adhoc_clients: Mutex<HashMap<String, Arc<Mutex<LSPClient>>>>,
    /// Workspace root directory for all LSP servers
    workspace_root: PathBuf,
}

impl LSPManager {
    /// Create a new LSP manager for the given workspace
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            adhoc_clients: Mutex::new(HashMap::new()),
            workspace_root: workspace_root.as_ref().to_path_buf(),
        }
    }

    /// Get or create an LSP client for the given language.
    ///
    /// If a client for this language already exists, it is returned.
    /// Otherwise, a new client is created and initialized.
    ///
    /// # Arguments
    /// * `language` - Language identifier (e.g., "go", "rust", "typescript")
    pub fn get_or_create_client(&self, language: &str) -> Result<Arc<Mutex<LSPClient>>> {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(client) = clients.get(language) {
            return Ok(Arc::clone(client));
        }

        let client = LSPClient::new(language, &self.workspace_root)?;
        let client_arc = Arc::new(Mutex::new(client));
        clients.insert(language.to_string(), Arc::clone(&client_arc));

        Ok(client_arc)
    }

    /// Get a client for a file by detecting its language
    pub fn get_client_for_file(&self, file_path: &str) -> Result<Arc<Mutex<LSPClient>>> {
        let language = super::client::detect_language(file_path)?;
        self.get_or_create_client(&language)
    }

    /// Get or create an LSP client for a non-default workspace root.
    /// Cached by "language:workspace_root" key to avoid leaking processes.
    pub fn get_or_create_adhoc_client(
        &self,
        language: &str,
        workspace_root: &Path,
    ) -> Result<Arc<Mutex<LSPClient>>> {
        let key = format!("{}:{}", language, workspace_root.display());
        let mut adhoc = self.adhoc_clients.lock().unwrap_or_else(|e| e.into_inner());

        if let Some(client) = adhoc.get(&key) {
            return Ok(Arc::clone(client));
        }

        let client = LSPClient::new(language, workspace_root)?;
        let client_arc = Arc::new(Mutex::new(client));
        adhoc.insert(key, Arc::clone(&client_arc));
        Ok(client_arc)
    }

    /// Get the workspace root this manager was initialized with
    pub fn workspace_root(&self) -> &Path {
        &self.workspace_root
    }

    /// Shutdown all active LSP servers (both default and ad-hoc)
    pub fn shutdown_all(&self) -> Result<()> {
        let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
        for (language, client_arc) in clients.drain() {
            let mut client = client_arc.lock().unwrap();
            if let Err(e) = client.shutdown() {
                eprintln!("Warning: Failed to shutdown {} server: {}", language, e);
            }
        }

        let mut adhoc = self.adhoc_clients.lock().unwrap_or_else(|e| e.into_inner());
        for (key, client_arc) in adhoc.drain() {
            let mut client = client_arc.lock().unwrap();
            if let Err(e) = client.shutdown() {
                eprintln!("Warning: Failed to shutdown ad-hoc server {}: {}", key, e);
            }
        }

        Ok(())
    }

    /// Get the number of active LSP servers
    pub fn active_count(&self) -> usize {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Check if a client exists for a given language
    pub fn has_client(&self, language: &str) -> bool {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).contains_key(language)
    }

    /// List all active languages
    pub fn active_languages(&self) -> Vec<String> {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).keys().cloned().collect()
    }
}

impl Drop for LSPManager {
    fn drop(&mut self) {
        let _ = self.shutdown_all();
    }
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_manager_creation() {
        let manager = LSPManager::new("/tmp");
        assert_eq!(manager.active_count(), 0);
        assert_eq!(manager.active_languages().len(), 0);
    }

    #[test]
    fn test_manager_has_client() {
        let manager = LSPManager::new("/tmp");
        assert!(!manager.has_client("go"));
        assert!(!manager.has_client("rust"));
    }

    #[test]
    fn test_active_languages() {
        let manager = LSPManager::new("/tmp");
        assert_eq!(manager.active_languages().len(), 0);
    }

    #[test]
    fn test_workspace_root() {
        let manager = LSPManager::new("/tmp/test-project");
        assert_eq!(manager.workspace_root(), Path::new("/tmp/test-project"));
    }
}
