// LSP Client Implementation
//
// Manages communication with a Language Server via JSON-RPC over stdin/stdout.

use super::types::*;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;
use thiserror::Error;

// ============================================================================
// Error Types
// ============================================================================

#[derive(Error, Debug)]
pub enum LSPError {
    #[error("LSP server not found: {binary} (language: {language})")]
    ServerNotFound { language: String, binary: String },

    #[error("Failed to start LSP server: {0}")]
    ServerStartFailed(String),

    #[error("Failed to initialize LSP server: {0}")]
    InitializeFailed(String),

    #[error("Request failed: {0}")]
    RequestFailed(String),

    #[error("Invalid response: {0}")]
    InvalidResponse(String),

    #[error("IO error: {0}")]
    IoError(#[from] std::io::Error),

    #[error("JSON error: {0}")]
    JsonError(#[from] serde_json::Error),

    #[error("Server returned error: {message} (code: {code})")]
    ServerError { code: i32, message: String },

    #[error("LSP request timed out after {0} seconds")]
    Timeout(u64),
}

pub type Result<T> = std::result::Result<T, LSPError>;

// ============================================================================
// LSP Client
// ============================================================================

/// Default timeout for LSP requests (30 seconds).
/// Pyright can take 10-20s on first indexing of large projects.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Timeout for initialization (60 seconds).
/// First initialization may trigger full project indexing.
const INIT_TIMEOUT: Duration = Duration::from_secs(60);

/// A parsed LSP message received from the server's stdout.
type StdoutMessage = std::result::Result<String, String>;

pub struct LSPClient {
    language: String,
    process: Child,
    stdin: ChildStdin,
    /// Channel receiver for parsed LSP messages from the background reader thread.
    message_rx: mpsc::Receiver<StdoutMessage>,
    request_id: Arc<AtomicU64>,
    request_timeout: Duration,
    /// Track documents already opened via didOpen to avoid duplicate notifications.
    opened_documents: HashSet<String>,
    /// Flag set to `false` when the stdout reader thread exits.
    /// Used to detect dead readers early and avoid 30s hangs in recv_timeout.
    reader_alive: Arc<AtomicBool>,
}

impl LSPClient {
    /// Create a new LSP client for the given language
    ///
    /// Supported languages:
    /// - "go" -> gopls
    /// - "rust" -> rust-analyzer
    /// - "typescript" / "javascript" -> typescript-language-server
    /// - "python" -> pyright-langserver
    ///
    /// Automatically resolves binaries from virtual environments (.venv, venv),
    /// VIRTUAL_ENV/CONDA_PREFIX env vars, and node_modules/.bin before falling
    /// back to system PATH. For Python projects with uv.lock, uses `uv run` to
    /// launch the language server.
    pub fn new(language: &str, workspace_root: &Path) -> Result<Self> {
        let (binary, args) = Self::get_server_command(language)?;
        let (final_binary, final_args) = Self::maybe_wrap_with_runner(language, &binary, args, workspace_root);
        let resolved_binary = Self::resolve_binary(&final_binary, workspace_root);

        tracing::debug!(
            "Starting LSP server: {} -> {} {:?} (workspace: {:?})",
            binary,
            resolved_binary,
            final_args,
            workspace_root
        );

        // Spawn the LSP server process
        let mut process = Command::new(&resolved_binary)
            .args(&final_args)
            .current_dir(workspace_root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())  // Suppress LSP server logs for now
            .spawn()
            .map_err(|e| {
                let mut msg = format!("{} {:?}: {}", resolved_binary, final_args, e);
                // CULTRA-975: add install hints when the binary isn't found.
                if e.kind() == std::io::ErrorKind::NotFound {
                    let hint = match language {
                        "python" => "Install with: pip install pyright (or: npm install -g pyright)",
                        "svelte" => "Install with: npm install -g svelte-language-server",
                        "typescript" | "javascript" => "Install with: npm install -g typescript-language-server typescript",
                        "rust" => "Install with: rustup component add rust-analyzer",
                        "go" => "Install with: go install golang.org/x/tools/gopls@latest",
                        _ => "Ensure the language server binary is in your PATH",
                    };
                    msg.push_str(&format!(". {}", hint));
                }
                LSPError::ServerStartFailed(msg)
            })?;

        let stdin = process
            .stdin
            .take()
            .ok_or_else(|| LSPError::ServerStartFailed("Failed to capture stdin".to_string()))?;

        let stdout = process
            .stdout
            .take()
            .ok_or_else(|| LSPError::ServerStartFailed("Failed to capture stdout".to_string()))?;

        // Spawn a background thread to read LSP messages from stdout.
        // This allows us to use recv_timeout for request timeouts instead of
        // blocking forever on a hung server.
        let reader_alive = Arc::new(AtomicBool::new(true));
        let message_rx = Self::spawn_stdout_reader(stdout, Arc::clone(&reader_alive));

        let mut client = Self {
            language: language.to_string(),
            process,
            stdin,
            message_rx,
            request_id: Arc::new(AtomicU64::new(1)),
            request_timeout: DEFAULT_REQUEST_TIMEOUT,
            opened_documents: HashSet::new(),
            reader_alive,
        };

        // Perform initialization handshake
        client.initialize(workspace_root)?;

        tracing::info!("LSP server initialized successfully: {}", language);

        Ok(client)
    }

    /// Get the LSP server command for a language
    fn get_server_command(language: &str) -> Result<(String, Vec<String>)> {
        match language {
            "go" => Ok(("gopls".to_string(), vec!["serve".to_string()])),
            "rust" => Ok(("rust-analyzer".to_string(), vec![])),
            "typescript" | "javascript" => Ok((
                "typescript-language-server".to_string(),
                vec!["--stdio".to_string()],
            )),
            "python" => Ok(("pyright-langserver".to_string(), vec!["--stdio".to_string()])),
            // CULTRA-972: svelte-language-server exposes a `svelteserver` binary
            // that speaks LSP over stdio, same pattern as typescript-language-server.
            "svelte" => Ok(("svelteserver".to_string(), vec!["--stdio".to_string()])),
            _ => Err(LSPError::ServerNotFound {
                language: language.to_string(),
                binary: "unknown".to_string(),
            }),
        }
    }

    /// Check if the language server should be launched via a project runner (uv, npx, etc.)
    /// instead of directly. Returns the (possibly rewritten) binary and args.
    ///
    /// Detection:
    /// - Python + uv.lock exists → `uv run <binary> <args>`
    /// - Python + Pipfile.lock exists → `pipenv run <binary> <args>`
    fn maybe_wrap_with_runner(
        language: &str,
        binary: &str,
        args: Vec<String>,
        workspace_root: &Path,
    ) -> (String, Vec<String>) {
        if language == "python" {
            // uv: check for uv.lock or pyproject.toml with [tool.uv]
            if workspace_root.join("uv.lock").exists() {
                let mut runner_args = vec!["run".to_string(), binary.to_string()];
                runner_args.extend(args);
                tracing::info!("Detected uv.lock — using `uv run {}` for Python LSP", binary);
                return ("uv".to_string(), runner_args);
            }

            // pipenv: check for Pipfile.lock
            if workspace_root.join("Pipfile.lock").exists() {
                let mut runner_args = vec!["run".to_string(), binary.to_string()];
                runner_args.extend(args);
                tracing::info!("Detected Pipfile.lock — using `pipenv run {}` for Python LSP", binary);
                return ("pipenv".to_string(), runner_args);
            }
        }

        // No runner detected — use binary directly
        (binary.to_string(), args)
    }

    /// Resolve binary path by checking virtual environments and local tool directories.
    ///
    /// Search order:
    /// 1. VIRTUAL_ENV env var (active venv)
    /// 2. CONDA_PREFIX env var (active conda env)
    /// 3. {workspace}/.venv/bin/ (common Python venv)
    /// 4. {workspace}/venv/bin/ (alternative Python venv)
    /// 5. {workspace}/node_modules/.bin/ (local npm packages)
    /// 6. Bare binary name (fall back to system PATH)
    fn resolve_binary(binary: &str, workspace_root: &Path) -> String {
        let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };

        // Check active virtual environment
        if let Ok(venv) = std::env::var("VIRTUAL_ENV") {
            let candidate = PathBuf::from(&venv).join(bin_dir).join(binary);
            if candidate.exists() {
                tracing::debug!("Found {} in VIRTUAL_ENV: {:?}", binary, candidate);
                return candidate.to_string_lossy().into_owned();
            }
        }

        // Check active conda environment
        if let Ok(conda) = std::env::var("CONDA_PREFIX") {
            let candidate = PathBuf::from(&conda).join(bin_dir).join(binary);
            if candidate.exists() {
                tracing::debug!("Found {} in CONDA_PREFIX: {:?}", binary, candidate);
                return candidate.to_string_lossy().into_owned();
            }
        }

        // Check common project-local venv directories
        for venv_dir in &[".venv", "venv"] {
            let candidate = workspace_root.join(venv_dir).join(bin_dir).join(binary);
            if candidate.exists() {
                tracing::debug!("Found {} in project venv: {:?}", binary, candidate);
                return candidate.to_string_lossy().into_owned();
            }
        }

        // Check node_modules/.bin (for typescript-language-server, pyright, etc.)
        let node_candidate = workspace_root.join("node_modules").join(".bin").join(binary);
        if node_candidate.exists() {
            tracing::debug!("Found {} in node_modules: {:?}", binary, node_candidate);
            return node_candidate.to_string_lossy().into_owned();
        }

        // Fall back to bare binary name (resolved via system PATH)
        binary.to_string()
    }

    /// Initialize the LSP server
    fn initialize(&mut self, workspace_root: &Path) -> Result<InitializeResult> {
        let root_uri = file_uri(&workspace_root.display().to_string());
        let workspace_name = workspace_root
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("workspace")
            .to_string();

        let params = InitializeParams {
            process_id: Some(std::process::id()),
            root_uri: Some(root_uri.clone()),
            capabilities: ClientCapabilities::default(),
            workspace_folders: Some(vec![WorkspaceFolder {
                uri: root_uri,
                name: workspace_name,
            }]),
        };

        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "initialize".to_string(),
            params: Some(json!(params)),
        };
        let request_json = serde_json::to_string(&request)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", request_json.len(), request_json)?;
        self.stdin.flush()?;

        // Use longer timeout for initialization (pyright may index entire project)
        let response_value = self.read_response_with_timeout(id, INIT_TIMEOUT)?;
        let result: InitializeResult = serde_json::from_value(response_value)
            .map_err(|e| LSPError::InvalidResponse(format!("Failed to parse initialize result: {}", e)))?;

        // Send initialized notification
        self.send_notification("initialized", Some(json!({})))?;

        Ok(result)
    }

    /// Notify the server that a document has been opened.
    ///
    /// Many LSP servers (notably pyright) require `textDocument/didOpen` before they
    /// will respond to queries about a file. This reads the file from disk and sends
    /// the notification with the full content.
    pub fn open_document(&mut self, file_path: &str) -> Result<()> {
        // Normalize path to prevent duplicate didOpen for symlinks/relative paths
        let canonical = std::fs::canonicalize(file_path)
            .unwrap_or_else(|_| std::path::PathBuf::from(file_path));
        let canonical_str = canonical.to_string_lossy().to_string();

        if self.opened_documents.contains(&canonical_str) {
            return Ok(());
        }

        let content = std::fs::read_to_string(file_path)
            .map_err(|e| LSPError::IoError(e))?;

        let language_id = super::client::detect_language(file_path)
            .unwrap_or_else(|_| "plaintext".to_string());

        self.send_notification(
            "textDocument/didOpen",
            Some(json!({
                "textDocument": {
                    "uri": file_uri(&canonical_str),
                    "languageId": language_id,
                    "version": 1,
                    "text": content
                }
            })),
        )?;

        self.opened_documents.insert(canonical_str.clone());
        tracing::debug!("Sent didOpen for: {} (canonical: {})", file_path, canonical_str);
        Ok(())
    }

    /// Send a JSON-RPC request and wait for response
    pub fn send_request(&mut self, method: &str, params: Option<Value>) -> Result<Value> {
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);

        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: method.to_string(),
            params,
        };

        tracing::debug!("LSP request: {} (id: {})", method, id);

        // Serialize request
        let request_json = serde_json::to_string(&request)?;

        // Write request with Content-Length header
        write!(
            self.stdin,
            "Content-Length: {}\r\n\r\n{}",
            request_json.len(),
            request_json
        )?;
        self.stdin.flush()?;

        // Read response
        self.read_response(id)
    }

    /// Send a JSON-RPC notification (no response expected)
    fn send_notification(&mut self, method: &str, params: Option<Value>) -> Result<()> {
        let notification = JsonRpcNotification {
            jsonrpc: "2.0".to_string(),
            method: method.to_string(),
            params,
        };

        let notification_json = serde_json::to_string(&notification)?;

        write!(
            self.stdin,
            "Content-Length: {}\r\n\r\n{}",
            notification_json.len(),
            notification_json
        )?;
        self.stdin.flush()?;

        Ok(())
    }

    /// Spawn a background thread that reads LSP messages from stdout and sends them
    /// through a channel. This decouples I/O from request handling, enabling timeouts.
    ///
    /// The `alive` flag is set to `false` when the thread exits (server crash, EOF,
    /// or receiver dropped), allowing `read_response_with_timeout` to detect a dead
    /// reader immediately instead of blocking for the full timeout.
    fn spawn_stdout_reader(stdout: ChildStdout, alive: Arc<AtomicBool>) -> mpsc::Receiver<StdoutMessage> {
        let (tx, rx) = mpsc::channel();
        std::thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                match Self::read_one_message(&mut reader) {
                    Ok(content) => {
                        if tx.send(Ok(content)).is_err() {
                            break; // Receiver dropped — client is gone
                        }
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break; // I/O error — stop reading
                    }
                }
            }
            alive.store(false, Ordering::Relaxed);
        });
        rx
    }

    /// Read a single LSP message (Content-Length header + body) from a buffered reader.
    fn read_one_message(reader: &mut BufReader<ChildStdout>) -> std::result::Result<String, String> {
        loop {
            let mut header = String::new();
            if reader.read_line(&mut header).map_err(|e| format!("stdout read error: {}", e))? == 0 {
                return Err("LSP server closed stdout (EOF)".to_string());
            }

            if header.trim().is_empty() {
                continue; // Skip blank lines between headers
            }

            if !header.starts_with("Content-Length:") {
                continue; // Skip non-Content-Length headers (e.g., Content-Type)
            }

            let content_length: usize = header
                .trim_start_matches("Content-Length:")
                .trim()
                .parse()
                .map_err(|_| format!("Invalid Content-Length: {}", header))?;

            const MAX_LSP_MESSAGE_SIZE: usize = 64 * 1024 * 1024; // 64 MiB
            if content_length > MAX_LSP_MESSAGE_SIZE {
                return Err(format!(
                    "Content-Length {} exceeds maximum of {} bytes",
                    content_length, MAX_LSP_MESSAGE_SIZE
                ));
            }

            // Skip remaining headers until blank line separating headers from body
            loop {
                let mut header_line = String::new();
                reader.read_line(&mut header_line).map_err(|e| format!("stdout read error: {}", e))?;
                if header_line.trim().is_empty() {
                    break;
                }
            }

            // Read body
            let mut content = vec![0u8; content_length];
            std::io::Read::read_exact(reader, &mut content)
                .map_err(|e| format!("stdout read error: {}", e))?;

            return String::from_utf8(content).map_err(|e| format!("Invalid UTF-8 in LSP response: {}", e));
        }
    }

    /// Read a JSON-RPC response with a timeout.
    fn read_response(&mut self, expected_id: u64) -> Result<Value> {
        self.read_response_with_timeout(expected_id, self.request_timeout)
    }

    /// Read a JSON-RPC response, waiting up to `timeout` for a matching response.
    fn read_response_with_timeout(&mut self, expected_id: u64, timeout: Duration) -> Result<Value> {
        let deadline = std::time::Instant::now() + timeout;

        loop {
            let remaining = deadline.saturating_duration_since(std::time::Instant::now());
            if remaining.is_zero() {
                return Err(LSPError::Timeout(timeout.as_secs()));
            }

            // If the reader thread is dead, use a short drain timeout instead of
            // blocking for the full remaining time (avoids 30s hangs on server crash).
            let wait_time = if self.reader_alive.load(Ordering::Relaxed) {
                remaining
            } else {
                Duration::from_millis(200)
            };

            let content_str = match self.message_rx.recv_timeout(wait_time) {
                Ok(Ok(msg)) => msg,
                Ok(Err(e)) => return Err(LSPError::InvalidResponse(e)),
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    return Err(LSPError::Timeout(timeout.as_secs()));
                }
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(LSPError::InvalidResponse(
                        "LSP server stdout reader disconnected (server may have crashed)".to_string(),
                    ));
                }
            };

            // Try to parse as response
            if let Ok(response) = serde_json::from_str::<JsonRpcResponse>(&content_str) {
                if response.id == serde_json::Value::from(expected_id) {
                    // Check for error
                    if let Some(error) = response.error {
                        return Err(LSPError::ServerError {
                            code: error.code,
                            message: error.message,
                        });
                    }

                    // Return result (null is valid — e.g., hover on whitespace)
                    return Ok(response.result.unwrap_or(Value::Null));
                }
            }

            // Notification or response to different request — keep draining
        }
    }

    /// Shutdown the LSP server gracefully with a short timeout.
    pub fn shutdown(&mut self) -> Result<()> {
        tracing::debug!("Shutting down LSP server: {}", self.language);

        // Send shutdown request with a 2-second timeout (instead of the default 30s)
        let id = self.request_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest {
            jsonrpc: "2.0".to_string(),
            id,
            method: "shutdown".to_string(),
            params: None,
        };
        let request_json = serde_json::to_string(&request)?;
        write!(self.stdin, "Content-Length: {}\r\n\r\n{}", request_json.len(), request_json)?;
        self.stdin.flush()?;
        let _ = self.read_response_with_timeout(id, Duration::from_secs(2));

        self.send_notification("exit", None)?;

        // Poll for process exit instead of sleeping a fixed duration
        for _ in 0..10 {
            if let Ok(Some(_)) = self.process.try_wait() {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Force kill if still running after 500ms of polling
        self.process.kill().ok();
        self.process.wait().ok();
        Ok(())
    }

    /// Get the language this client is for
    pub fn language(&self) -> &str {
        &self.language
    }
}

impl Drop for LSPClient {
    fn drop(&mut self) {
        let _ = self.shutdown();
    }
}

// ============================================================================
// Helper Functions
// ============================================================================

/// Create a properly percent-encoded file URI from an absolute path (RFC-3986).
///
/// Each path segment is individually percent-encoded using the `urlencoding` crate,
/// preserving `/` separators. Handles Windows paths by converting backslashes and
/// adding the extra `/` prefix required for drive letter URIs.
pub fn file_uri(path: &str) -> String {
    // Normalize Windows backslashes to forward slashes
    let normalized = path.replace('\\', "/");

    // Percent-encode each path segment individually (preserve `/`)
    let encoded: String = normalized
        .split('/')
        .map(|segment| urlencoding::encode(segment).into_owned())
        .collect::<Vec<_>>()
        .join("/");

    // Windows drive letter paths need file:///C:/...
    if encoded.chars().nth(1) == Some(':') || encoded.starts_with("%3A") {
        format!("file:///{}", encoded)
    } else {
        format!("file://{}", encoded)
    }
}

/// Detect programming language from file extension
pub fn detect_language(file_path: &str) -> Result<String> {
    let path = Path::new(file_path);
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .ok_or_else(|| {
            LSPError::InvalidResponse(format!("Cannot detect language for file: {}", file_path))
        })?;

    match extension {
        "go" => Ok("go".to_string()),
        "rs" => Ok("rust".to_string()),
        "ts" | "tsx" => Ok("typescript".to_string()),
        "js" | "jsx" => Ok("typescript".to_string()),
        "py" => Ok("python".to_string()),
        "svelte" => Ok("svelte".to_string()),
        _ => Err(LSPError::InvalidResponse(format!(
            "Unsupported file extension: {}",
            extension
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::env;

    #[test]
    fn test_detect_language() {
        assert_eq!(detect_language("/path/to/file.go").unwrap(), "go");
        assert_eq!(detect_language("/path/to/file.rs").unwrap(), "rust");
        assert_eq!(detect_language("/path/to/file.ts").unwrap(), "typescript");
        assert_eq!(detect_language("/path/to/file.tsx").unwrap(), "typescript");
        assert_eq!(detect_language("/path/to/file.js").unwrap(), "typescript");
        assert_eq!(detect_language("/path/to/file.jsx").unwrap(), "typescript");
        assert_eq!(detect_language("/path/to/file.py").unwrap(), "python");
        assert!(detect_language("/path/to/file.txt").is_err());
        assert!(detect_language("/path/to/no_extension").is_err());
    }

    #[test]
    fn test_get_server_command() {
        let (binary, args) = LSPClient::get_server_command("go").unwrap();
        assert_eq!(binary, "gopls");
        assert_eq!(args, vec!["serve"]);

        let (binary, args) = LSPClient::get_server_command("rust").unwrap();
        assert_eq!(binary, "rust-analyzer");
        assert_eq!(args.len(), 0);

        let (binary, args) = LSPClient::get_server_command("typescript").unwrap();
        assert_eq!(binary, "typescript-language-server");
        assert_eq!(args, vec!["--stdio"]);

        let (binary, args) = LSPClient::get_server_command("python").unwrap();
        assert_eq!(binary, "pyright-langserver");
        assert_eq!(args, vec!["--stdio"]);

        assert!(LSPClient::get_server_command("unsupported").is_err());
    }

    #[test]
    fn test_resolve_binary_falls_back_to_bare_name() {
        // When no venv exists, should return the bare binary name
        let workspace = Path::new("/nonexistent/workspace");
        assert_eq!(LSPClient::resolve_binary("gopls", workspace), "gopls");
    }

    #[test]
    fn test_resolve_binary_finds_venv() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-venv-resolve");
        let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };
        let venv_bin = tmp.join(".venv").join(bin_dir);
        fs::create_dir_all(&venv_bin).unwrap();

        let fake_binary = venv_bin.join("pyright-langserver");
        fs::write(&fake_binary, "").unwrap();

        let resolved = LSPClient::resolve_binary("pyright-langserver", &tmp);
        assert_eq!(resolved, fake_binary.to_string_lossy().to_string());

        // Cleanup
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_resolve_binary_prefers_virtual_env_over_project_venv() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-venv-priority");
        let bin_dir = if cfg!(windows) { "Scripts" } else { "bin" };

        // Create both a VIRTUAL_ENV path and a project .venv
        let external_venv = tmp.join("external-venv").join(bin_dir);
        let project_venv = tmp.join("project").join(".venv").join(bin_dir);
        fs::create_dir_all(&external_venv).unwrap();
        fs::create_dir_all(&project_venv).unwrap();

        let ext_binary = external_venv.join("pyright-langserver");
        let proj_binary = project_venv.join("pyright-langserver");
        fs::write(&ext_binary, "").unwrap();
        fs::write(&proj_binary, "").unwrap();

        // Set VIRTUAL_ENV to external
        let prev = std::env::var("VIRTUAL_ENV").ok();
        std::env::set_var("VIRTUAL_ENV", tmp.join("external-venv"));

        let resolved = LSPClient::resolve_binary("pyright-langserver", &tmp.join("project"));
        assert_eq!(resolved, ext_binary.to_string_lossy().to_string());

        // Restore env
        match prev {
            Some(val) => std::env::set_var("VIRTUAL_ENV", val),
            None => std::env::remove_var("VIRTUAL_ENV"),
        }

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_maybe_wrap_with_uv_when_lock_exists() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-uv-wrap");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create uv.lock
        fs::write(tmp.join("uv.lock"), "").unwrap();

        let (binary, args) = LSPClient::maybe_wrap_with_runner(
            "python",
            "pyright-langserver",
            vec!["--stdio".to_string()],
            &tmp,
        );
        assert_eq!(binary, "uv");
        assert_eq!(args, vec!["run", "pyright-langserver", "--stdio"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_maybe_wrap_no_lock_file() {
        // No uv.lock → pass through unchanged
        let workspace = Path::new("/nonexistent/workspace");
        let (binary, args) = LSPClient::maybe_wrap_with_runner(
            "python",
            "pyright-langserver",
            vec!["--stdio".to_string()],
            workspace,
        );
        assert_eq!(binary, "pyright-langserver");
        assert_eq!(args, vec!["--stdio"]);
    }

    #[test]
    fn test_maybe_wrap_ignores_non_python() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-uv-non-python");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();
        fs::write(tmp.join("uv.lock"), "").unwrap();

        // Go should not be wrapped even if uv.lock exists
        let (binary, args) = LSPClient::maybe_wrap_with_runner(
            "go",
            "gopls",
            vec!["serve".to_string()],
            &tmp,
        );
        assert_eq!(binary, "gopls");
        assert_eq!(args, vec!["serve"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_maybe_wrap_with_pipenv_when_pipfile_lock_exists() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-pipenv-wrap");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Create Pipfile.lock (no uv.lock)
        fs::write(tmp.join("Pipfile.lock"), "{}").unwrap();

        let (binary, args) = LSPClient::maybe_wrap_with_runner(
            "python",
            "pyright-langserver",
            vec!["--stdio".to_string()],
            &tmp,
        );
        assert_eq!(binary, "pipenv");
        assert_eq!(args, vec!["run", "pyright-langserver", "--stdio"]);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_uv_takes_priority_over_pipenv() {
        use std::fs;

        let tmp = env::temp_dir().join("cultra-test-uv-priority");
        let _ = fs::remove_dir_all(&tmp);
        fs::create_dir_all(&tmp).unwrap();

        // Both exist — uv should win
        fs::write(tmp.join("uv.lock"), "").unwrap();
        fs::write(tmp.join("Pipfile.lock"), "{}").unwrap();

        let (binary, _args) = LSPClient::maybe_wrap_with_runner(
            "python",
            "pyright-langserver",
            vec!["--stdio".to_string()],
            &tmp,
        );
        assert_eq!(binary, "uv");

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_lsp_error_display() {
        let err = LSPError::ServerNotFound {
            language: "go".to_string(),
            binary: "gopls".to_string(),
        };
        assert!(err.to_string().contains("gopls"));
        assert!(err.to_string().contains("go"));

        let err = LSPError::ServerError {
            code: -32600,
            message: "Invalid request".to_string(),
        };
        assert!(err.to_string().contains("-32600"));
        assert!(err.to_string().contains("Invalid request"));
    }

    // Integration test - requires gopls to be installed
    #[test]
    #[ignore] // Run with `cargo test -- --ignored`
    fn test_lsp_client_initialization() {
        // Check if gopls is available
        if which::which("gopls").is_err() {
            eprintln!("Skipping test: gopls not found in PATH");
            return;
        }

        let workspace_root = env::current_dir().unwrap();
        let result = LSPClient::new("go", &workspace_root);

        match result {
            Ok(mut client) => {
                // Client initialized successfully
                assert_eq!(client.language(), "go");

                // Test shutdown
                assert!(client.shutdown().is_ok());
            }
            Err(e) => {
                // If it fails due to gopls not found, that's expected
                if matches!(e, LSPError::ServerStartFailed(_)) {
                    eprintln!("gopls found but failed to start: {}", e);
                } else {
                    panic!("Unexpected error: {}", e);
                }
            }
        }
    }
}
