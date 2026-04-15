// LSP Server Manager
//
// Manages multiple LSP server lifecycles, keeping them alive across requests
// and providing lazy initialization for each language.

use super::client::{LSPClient, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime};

// ============================================================================
// Warmup state (CULTRA-950, fixed in CULTRA-952)
// ============================================================================

/// Per-language, per-crate warmup cache entry. Cached on the LSPManager so
/// a single session pays the cargo-check / go-build cost at most once per
/// "fresh" crate state — see invalidation logic in `ensure_warm`.
///
/// CULTRA-952: also caches FAILED warmups so a retry storm within the same
/// edit cycle doesn't replay the same 30s `cargo check` failure on every
/// call. Failures invalidate on the same mtime trigger as successes, so
/// "I fixed the build, now retry" works automatically.
#[derive(Debug, Clone)]
pub struct WarmupCacheEntry {
    /// Maximum mtime observed across the crate's source files at the
    /// moment warmup ran. Used as the invalidation stamp: if any source
    /// file has been touched since, the cache is stale and we re-warm.
    pub mtime_stamp: SystemTime,
    /// Wall-clock time the warmup command took.
    pub elapsed_ms: u64,
    /// "warm" or "failed" — what the original outcome was. Replayed back
    /// to callers on cache hits.
    pub status: String,
    /// Command string actually executed (resolved with manifest path).
    pub command: Option<String>,
    /// Failure message if the original warmup failed; None on success.
    pub message: Option<String>,
}

/// Result of an `ensure_warm` call. Returned to callers and surfaced in the
/// tool response so the agent can see the cost and the cache state.
///
/// IMPORTANT (Vestige, post-CULTRA-952 verification): `status` describes the
/// warmup *command* (cargo check / go build / tsc --noEmit) — NOT the LSP
/// server's reference-index readiness. Those have independent timelines.
/// `cargo check` blocks on compilation; rust-analyzer's reference database
/// is populated *asynchronously* after it sees the target-dir metadata. The
/// window between the two is typically 20-60s on a medium workspace, so a
/// `status: "warm"` warmup followed by an immediate `find_references` call
/// can still return `lsp_index_status: "cold"`. Callers should treat the
/// combination of `warmup_report.status == "warm"` AND top-level
/// `lsp_index_status == "cold"` as a transient race and retry after ~30s.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WarmupReport {
    pub language: String,
    /// "warm"   — ran the warmup command, command succeeded. Does NOT mean
    ///            the LSP reference database is queryable yet — see the
    ///            struct-level note above.
    /// "cached" — hit the per-session cache (look at `cached_status` for the
    ///            original outcome — could be a cached success or a cached
    ///            failure replayed)
    /// "skipped"— no warmup command available for this language, OR no
    ///            manifest could be resolved between file_path and the
    ///            sandbox root (graceful)
    /// "failed" — command ran but exited non-zero or failed to spawn
    pub status: String,
    pub cached: bool,
    pub elapsed_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// CULTRA-952: when status='cached', this records the original outcome
    /// the cache entry was built from ('warm' or 'failed'). Lets callers
    /// distinguish a cached success from a cached failure without parsing
    /// the message field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cached_status: Option<String>,
    /// CULTRA-952: resolved manifest directory the warmup ran against.
    /// Surfaced so callers can verify the right crate was warmed in
    /// monorepo / multi-crate layouts.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manifest_dir: Option<String>,
}

/// CULTRA-952: a resolved warmup target — language plus the directory of
/// the manifest the warmup command will run against. Constructed by walking
/// up from a source file looking for the language's manifest, capped at the
/// sandbox root so the search can never escape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupTarget {
    pub language: String,
    /// Absolute path to the directory containing the manifest. The warmup
    /// command runs as if this were cwd (via --manifest-path / -C / --project,
    /// so the actual subprocess cwd is irrelevant).
    pub manifest_dir: PathBuf,
}

/// CULTRA-952: filename of the language's manifest (Cargo.toml, go.mod,
/// tsconfig.json). Returns None for languages with no warmup story.
pub fn manifest_filename_for_language(language: &str) -> Option<&'static str> {
    match language {
        "rust" => Some("Cargo.toml"),
        "go" => Some("go.mod"),
        // For TS/JS we look for tsconfig.json. If a JS project has only
        // package.json and no tsconfig, we skip — no clean way to "warm"
        // the LSP without a TypeScript compiler config.
        "typescript" | "tsx" | "javascript" | "jsx" => Some("tsconfig.json"),
        // CULTRA-972: Svelte projects anchor on svelte.config.js for workspace
        // resolution. Warmup is handled by svelteserver's internal tsserver.
        "svelte" => Some("svelte.config.js"),
        // Python: pyright IS the LSP. Running it from CLI doesn't pre-warm
        // the same in-memory state. Skip.
        _ => None,
    }
}

/// CULTRA-952: resolve the warmup target for `language` by walking up from
/// `file_path` to find the language's manifest. Stops at `sandbox_root`.
///
/// CULTRA-954/955 refactor: delegates to the shared `crate::workspace`
/// resolver. Same logic, single source of truth for the walk-up + sandbox
/// guard. The warmup-specific bit (which manifest filename for which
/// language) lives in `manifest_filename_for_language` above and is
/// re-stated here as a `WorkspaceAnchor::Manifest(&[...])` so the warmup
/// path can stay decoupled from the LSP-workspace anchor logic (which
/// has different priorities — e.g. tsconfig.json before package.json for
/// LSP, but only tsconfig.json for `tsc --project` warmup).
pub fn resolve_warmup_target(
    language: &str,
    file_path: &Path,
    sandbox_root: &Path,
) -> Option<WarmupTarget> {
    use crate::workspace::{resolve_workspace_root, WorkspaceAnchor};

    // Match the manifest_filename_for_language table — wrap each as a
    // single-entry Manifest anchor so the walk-up is identical to the
    // CULTRA-952 logic.
    static RUST: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["Cargo.toml"]);
    static GO: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["go.mod"]);
    static TS: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["tsconfig.json"]);

    let anchor: &WorkspaceAnchor = match language {
        "rust" => &RUST,
        "go" => &GO,
        "typescript" | "tsx" | "javascript" | "jsx" => &TS,
        _ => return None,
    };

    let resolved = resolve_workspace_root(file_path, anchor, sandbox_root)?;
    Some(WarmupTarget {
        language: language.to_string(),
        manifest_dir: resolved.root,
    })
}

/// CULTRA-952: build the warmup command for a resolved target. Each
/// language uses its native "run as if cwd were over there" affordance —
/// `cargo --manifest-path`, `go -C <dir>`, `tsc --project <dir>` — so the
/// command works regardless of the subprocess's actual cwd.
pub fn warmup_command_for_target(target: &WarmupTarget) -> Option<(String, Vec<String>)> {
    let dir_str = target.manifest_dir.to_string_lossy().to_string();
    match target.language.as_str() {
        "rust" => {
            let manifest = target.manifest_dir.join("Cargo.toml").to_string_lossy().to_string();
            Some((
                "cargo".to_string(),
                vec![
                    "check".to_string(),
                    "--manifest-path".to_string(),
                    manifest,
                    "--quiet".to_string(),
                ],
            ))
        }
        "go" => Some((
            "go".to_string(),
            vec!["build".to_string(), "-C".to_string(), dir_str, "./...".to_string()],
        )),
        "typescript" | "tsx" | "javascript" | "jsx" => Some((
            "tsc".to_string(),
            vec!["--noEmit".to_string(), "--project".to_string(), dir_str],
        )),
        _ => None,
    }
}

/// Walk the workspace and return the latest mtime across source files
/// relevant to `language`. Used as the cache-invalidation stamp: if a later
/// `ensure_warm` call sees a higher max-mtime, the workspace has been edited
/// since the last warmup and we need to re-warm.
///
/// Vestige's pushback: "warm once, stay warm forever" is wrong — long
/// edit-and-rebuild sessions silently drift into stale-index territory.
/// This function is the invalidation lever.
pub fn max_workspace_source_mtime(root: &Path, language: &str) -> Option<SystemTime> {
    let extensions: &[&str] = match language {
        "rust" => &["rs", "toml"],
        "go" => &["go", "mod", "sum"],
        "typescript" | "tsx" | "javascript" | "jsx" => &["ts", "tsx", "js", "jsx", "json"],
        "python" => &["py"],
        _ => return None,
    };

    fn walk(path: &Path, extensions: &[&str], best: &mut Option<SystemTime>) {
        if path.is_dir() {
            // Skip the common ignore set. Mirrors collect_component_files in
            // mcp/tools.rs — keep these in sync if either is updated.
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                if matches!(
                    name,
                    "node_modules" | ".git" | "dist" | "build" | "target"
                    | ".next" | "__pycache__" | ".venv" | "vendor"
                ) {
                    return;
                }
            }
            if let Ok(entries) = std::fs::read_dir(path) {
                for entry in entries.flatten() {
                    walk(&entry.path(), extensions, best);
                }
            }
        } else if path.is_file() {
            let ext_match = path
                .extension()
                .and_then(|e| e.to_str())
                .map(|e| extensions.contains(&e))
                .unwrap_or(false);
            // Also accept exact filenames like "Cargo.toml" / "go.mod" — the
            // ext-based check above already handles "toml"/"mod" extensions
            // but we double-check filename matches in case some workspace
            // uses non-standard naming.
            let name_match = path.file_name().and_then(|n| n.to_str())
                .map(|n| matches!(n, "Cargo.toml" | "go.mod" | "go.sum"))
                .unwrap_or(false);
            if ext_match || name_match {
                if let Ok(meta) = path.metadata() {
                    if let Ok(mtime) = meta.modified() {
                        *best = Some(match *best {
                            Some(prev) if prev >= mtime => prev,
                            _ => mtime,
                        });
                    }
                }
            }
        }
    }

    let mut best = None;
    walk(root, extensions, &mut best);
    best
}

/// CULTRA-950: spawn a subprocess with a wall-clock timeout. Polls
/// `try_wait` every 100ms; on timeout, kills the child and returns an error.
/// Bounded to keep a stuck warmup command from hanging the tool call.
///
/// Used by `LSPManager::ensure_warm` to bound `cargo check` / `go build` /
/// `tsc --noEmit`. Kept private to this module — callers go through
/// `ensure_warm`.
fn run_with_timeout(
    cmd: &str,
    args: &[&str],
    cwd: &Path,
    timeout: Duration,
) -> std::result::Result<(), String> {
    use std::process::{Command, Stdio};

    let mut child = Command::new(cmd)
        .args(args)
        .current_dir(cwd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("failed to spawn '{}': {}", cmd, e))?;

    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                if status.success() {
                    return Ok(());
                }
                // Drain stderr for diagnostics — bounded by Stdio::piped buffer.
                let mut stderr_text = String::new();
                if let Some(mut s) = child.stderr.take() {
                    use std::io::Read;
                    let _ = s.read_to_string(&mut stderr_text);
                }
                let snippet: String = stderr_text.chars().take(200).collect();
                return Err(format!(
                    "'{}' exited with status {}: {}",
                    cmd,
                    status.code().map(|c| c.to_string()).unwrap_or_else(|| "<signal>".to_string()),
                    snippet.trim()
                ));
            }
            Ok(None) => {
                if start.elapsed() >= timeout {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(format!("'{}' exceeded {}s timeout", cmd, timeout.as_secs()));
                }
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(format!("error polling '{}': {}", cmd, e)),
        }
    }
}

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
    /// CULTRA-950 / CULTRA-952: per-(language, crate-dir) warmup cache.
    /// Keyed by `format!("{}:{}", language, manifest_dir.display())` so a
    /// monorepo with five Rust crates has five independent cache entries
    /// instead of one global "rust" entry that incorrectly applies to all
    /// of them. Behind a Mutex so concurrent tool calls don't double-warm.
    warmup: Mutex<HashMap<String, WarmupCacheEntry>>,
}

impl LSPManager {
    /// Create a new LSP manager for the given workspace
    pub fn new<P: AsRef<Path>>(workspace_root: P) -> Self {
        Self {
            clients: Mutex::new(HashMap::new()),
            adhoc_clients: Mutex::new(HashMap::new()),
            workspace_root: workspace_root.as_ref().to_path_buf(),
            warmup: Mutex::new(HashMap::new()),
        }
    }

    /// CULTRA-950 / CULTRA-952: ensure the LSP index for the crate that
    /// contains `file_path` is warm. First call walks up from `file_path`
    /// to find the language's manifest (Cargo.toml / go.mod / tsconfig.json),
    /// then runs the warmup command via the language's "run as if cwd were
    /// over there" flag (`--manifest-path`, `-C`, `--project`) so the
    /// command is independent of the actual subprocess cwd.
    ///
    /// Subsequent calls within the same session hit the per-(language, crate)
    /// cache and return instantly. The cache invalidates when any source
    /// file in the crate is newer than the warmup stamp — Vestige's
    /// follow-up: "warm once, stay warm forever" is wrong, long edit
    /// sessions silently drift into stale-index territory otherwise.
    ///
    /// CULTRA-952: failed warmups are also cached (with the same mtime
    /// invalidation) so a retry storm within a single edit cycle doesn't
    /// replay the same multi-second failure on every call.
    ///
    /// Languages with no warmup command, OR supported languages where no
    /// manifest can be resolved between `file_path` and the sandbox root,
    /// return a `skipped` status rather than an error.
    pub fn ensure_warm(&self, language: &str, file_path: &Path) -> WarmupReport {
        // Step 1: resolve the warmup target by walking up to the manifest.
        let target = match resolve_warmup_target(language, file_path, &self.workspace_root) {
            Some(t) => t,
            None => {
                let reason = if manifest_filename_for_language(language).is_none() {
                    format!(
                        "No warmup command configured for language '{}'. Pass require_warm_index=true if cold-index handling is critical.",
                        language
                    )
                } else {
                    format!(
                        "No {} manifest found between {} and the sandbox root. \
                         Add a manifest at the crate root or skip warmup for this file.",
                        manifest_filename_for_language(language).unwrap_or("manifest"),
                        file_path.display()
                    )
                };
                return WarmupReport {
                    language: language.to_string(),
                    status: "skipped".to_string(),
                    cached: false,
                    elapsed_ms: 0,
                    command: None,
                    message: Some(reason),
                    cached_status: None,
                    manifest_dir: None,
                };
            }
        };

        let (cmd, cmd_args) = match warmup_command_for_target(&target) {
            Some(t) => t,
            None => {
                // Should not happen — manifest_filename_for_language and
                // warmup_command_for_target stay in sync — but handle it.
                return WarmupReport {
                    language: language.to_string(),
                    status: "skipped".to_string(),
                    cached: false,
                    elapsed_ms: 0,
                    command: None,
                    message: Some(format!("No warmup command builder for language '{}'", language)),
                    cached_status: None,
                    manifest_dir: Some(target.manifest_dir.display().to_string()),
                };
            }
        };

        let cache_key = format!("{}:{}", language, target.manifest_dir.display());
        let display_command = format!("{} {}", cmd, cmd_args.join(" "));

        // Step 2: check the cache. mtime invalidation is scoped to the
        // crate dir, not the whole sandbox — editing a sibling crate
        // shouldn't invalidate this crate's warmup.
        let current_max_mtime = max_workspace_source_mtime(&target.manifest_dir, language);
        {
            let warmup = self.warmup.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(entry) = warmup.get(&cache_key) {
                let still_fresh = match current_max_mtime {
                    Some(now) => now <= entry.mtime_stamp,
                    None => true, // No source files found → trust cache.
                };
                if still_fresh {
                    return WarmupReport {
                        language: language.to_string(),
                        status: "cached".to_string(),
                        cached: true,
                        elapsed_ms: entry.elapsed_ms,
                        command: entry.command.clone(),
                        message: entry.message.clone(),
                        cached_status: Some(entry.status.clone()),
                        manifest_dir: Some(target.manifest_dir.display().to_string()),
                    };
                }
            }
        }

        // Step 3: run the warmup command. Default timeout is 60s. For large
        // projects (e.g. Grafana: 5871 Go files) this isn't enough — set
        // CULTRA_WARMUP_TIMEOUT_SECS to increase it.
        let timeout_secs: u64 = std::env::var("CULTRA_WARMUP_TIMEOUT_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let cmd_args_refs: Vec<&str> = cmd_args.iter().map(|s| s.as_str()).collect();
        let start = Instant::now();
        let exec_result = run_with_timeout(
            &cmd,
            &cmd_args_refs,
            &target.manifest_dir,
            Duration::from_secs(timeout_secs),
        );
        let elapsed_ms = start.elapsed().as_millis() as u64;

        // Step 4: cache the outcome — success OR failure (CULTRA-952).
        // Stamp with the mtime observed BEFORE running the command, so any
        // edits made during the warmup window correctly invalidate on the
        // next call.
        let stamp = current_max_mtime.unwrap_or_else(SystemTime::now);

        let (status, message) = match &exec_result {
            Ok(()) => ("warm".to_string(), None),
            Err(e) => (
                "failed".to_string(),
                Some(format!(
                    "Warmup command failed: {}. The LSP may still be partially indexed; results will be best-effort.",
                    e
                )),
            ),
        };

        {
            let mut warmup = self.warmup.lock().unwrap_or_else(|e| e.into_inner());
            warmup.insert(cache_key, WarmupCacheEntry {
                mtime_stamp: stamp,
                elapsed_ms,
                status: status.clone(),
                command: Some(display_command.clone()),
                message: message.clone(),
            });
        }

        WarmupReport {
            language: language.to_string(),
            status,
            cached: false,
            elapsed_ms,
            command: Some(display_command),
            message,
            cached_status: None,
            manifest_dir: Some(target.manifest_dir.display().to_string()),
        }
    }

    /// CULTRA-950 / CULTRA-952 test-only helper: insert a cache entry
    /// directly so cache-hit tests don't have to invoke real subprocesses.
    #[cfg(test)]
    pub fn inject_warmup_cache_entry(&self, language: &str, manifest_dir: &Path, entry: WarmupCacheEntry) {
        let key = format!("{}:{}", language, manifest_dir.display());
        let mut warmup = self.warmup.lock().unwrap_or_else(|e| e.into_inner());
        warmup.insert(key, entry);
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

    // CULTRA-950 / CULTRA-952: warmup tests.

    #[test]
    fn test_manifest_filename_for_known_languages() {
        assert_eq!(manifest_filename_for_language("rust"), Some("Cargo.toml"));
        assert_eq!(manifest_filename_for_language("go"), Some("go.mod"));
        assert_eq!(manifest_filename_for_language("typescript"), Some("tsconfig.json"));
        assert_eq!(manifest_filename_for_language("tsx"), Some("tsconfig.json"));
        assert_eq!(manifest_filename_for_language("javascript"), Some("tsconfig.json"));
    }

    #[test]
    fn test_manifest_filename_for_unknown_language_returns_none() {
        assert!(manifest_filename_for_language("python").is_none());
        assert!(manifest_filename_for_language("ruby").is_none());
        assert!(manifest_filename_for_language("brainfuck").is_none());
    }

    #[test]
    fn test_warmup_command_for_target_rust_uses_manifest_path() {
        // CULTRA-952: rust must use --manifest-path so the command is
        // independent of the subprocess cwd.
        let target = WarmupTarget {
            language: "rust".to_string(),
            manifest_dir: PathBuf::from("/some/crate"),
        };
        let (cmd, args) = warmup_command_for_target(&target).unwrap();
        assert_eq!(cmd, "cargo");
        assert!(args.iter().any(|a| a == "check"));
        assert!(args.iter().any(|a| a == "--manifest-path"),
            "rust warmup MUST use --manifest-path: {:?}", args);
        assert!(args.iter().any(|a| a.ends_with("Cargo.toml")),
            "args should include the resolved Cargo.toml path: {:?}", args);
        assert!(args.iter().any(|a| a == "--quiet"));
    }

    #[test]
    fn test_warmup_command_for_target_go_uses_dash_c() {
        // CULTRA-952: go must use -C <dir>.
        let target = WarmupTarget {
            language: "go".to_string(),
            manifest_dir: PathBuf::from("/some/module"),
        };
        let (cmd, args) = warmup_command_for_target(&target).unwrap();
        assert_eq!(cmd, "go");
        assert!(args.iter().any(|a| a == "-C"),
            "go warmup MUST use -C: {:?}", args);
        assert!(args.iter().any(|a| a == "/some/module"));
        assert!(args.iter().any(|a| a == "./..."));
    }

    #[test]
    fn test_warmup_command_for_target_typescript_uses_project() {
        let target = WarmupTarget {
            language: "typescript".to_string(),
            manifest_dir: PathBuf::from("/some/ts/project"),
        };
        let (cmd, args) = warmup_command_for_target(&target).unwrap();
        assert_eq!(cmd, "tsc");
        assert!(args.iter().any(|a| a == "--project"));
        assert!(args.iter().any(|a| a == "/some/ts/project"));
    }

    #[test]
    fn test_resolve_warmup_target_finds_manifest_in_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().to_path_buf();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let src = crate_dir.join("main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();

        let target = resolve_warmup_target("rust", &src, dir.path()).unwrap();
        assert_eq!(target.language, "rust");
        assert_eq!(
            target.manifest_dir.canonicalize().unwrap(),
            crate_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_warmup_target_walks_up_to_parent() {
        // Sandbox: /tmp/X
        // Crate root: /tmp/X/sub      (Cargo.toml here)
        // Source file: /tmp/X/sub/src/lib.rs
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().join("sub");
        let src_dir = crate_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let src = src_dir.join("lib.rs");
        std::fs::write(&src, "pub fn x() {}\n").unwrap();

        let target = resolve_warmup_target("rust", &src, dir.path()).unwrap();
        assert_eq!(
            target.manifest_dir.canonicalize().unwrap(),
            crate_dir.canonicalize().unwrap(),
            "should resolve to the crate dir, not the src dir"
        );
    }

    #[test]
    fn test_resolve_warmup_target_walks_up_through_multiple_dirs() {
        // CULTRA-952 reproduction: sandbox at /vestige, crate at /vestige/vux,
        // file at /vestige/vux/src/compositor.rs. Walking up from src/ must
        // find vux/Cargo.toml.
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().join("vux");
        let src_dir = crate_dir.join("src");
        let nested_dir = src_dir.join("foo").join("bar");
        std::fs::create_dir_all(&nested_dir).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"vux\"\n").unwrap();
        let src = nested_dir.join("deep.rs");
        std::fs::write(&src, "pub fn x() {}\n").unwrap();

        let target = resolve_warmup_target("rust", &src, dir.path()).unwrap();
        assert_eq!(
            target.manifest_dir.canonicalize().unwrap(),
            crate_dir.canonicalize().unwrap()
        );
    }

    #[test]
    fn test_resolve_warmup_target_returns_none_when_no_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("orphan.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();
        let target = resolve_warmup_target("rust", &src, dir.path());
        assert!(target.is_none(),
            "should return None when no Cargo.toml is found below the sandbox root");
    }

    #[test]
    fn test_resolve_warmup_target_does_not_escape_sandbox() {
        // Even if a Cargo.toml exists ABOVE the sandbox root, walking up
        // must stop at the sandbox boundary.
        let outer = tempfile::tempdir().unwrap();
        let sandbox = outer.path().join("sandbox");
        let src_dir = sandbox.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        // Place a Cargo.toml ABOVE the sandbox — this must be ignored.
        std::fs::write(outer.path().join("Cargo.toml"), "[package]\nname=\"escape\"\n").unwrap();
        let src = src_dir.join("a.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();

        let target = resolve_warmup_target("rust", &src, &sandbox);
        assert!(target.is_none(),
            "must NOT walk above sandbox root to find a manifest, got: {:?}", target);
    }

    #[test]
    fn test_resolve_warmup_target_unsupported_language_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "def x(): pass\n").unwrap();
        assert!(resolve_warmup_target("python", &src, dir.path()).is_none());
    }

    #[test]
    fn test_max_workspace_source_mtime_finds_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("main.go");
        std::fs::write(&path, "package main\n").unwrap();

        let mtime = max_workspace_source_mtime(dir.path(), "go");
        assert!(mtime.is_some(), "should find the .go file");
    }

    #[test]
    fn test_max_workspace_source_mtime_skips_ignore_dirs() {
        // Files inside node_modules / target / .git must NOT contribute to
        // the mtime stamp — otherwise rebuilds inside target/ would
        // constantly invalidate the cache.
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir_all(&target).unwrap();
        std::fs::write(target.join("buried.rs"), "fn x() {}").unwrap();

        let mtime = max_workspace_source_mtime(dir.path(), "rust");
        assert!(mtime.is_none(),
            "files under target/ should be skipped, got mtime: {:?}", mtime);
    }

    #[test]
    fn test_max_workspace_source_mtime_returns_none_for_unknown_lang() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.rs"), "fn x() {}").unwrap();
        assert!(max_workspace_source_mtime(dir.path(), "cobol").is_none());
    }

    #[test]
    fn test_max_workspace_source_mtime_picks_latest() {
        // Two files, the second touched after a delay → mtime should match
        // the second. Sleep 50ms to make sure mtimes are distinguishable on
        // filesystems with low-res timestamps.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.go"), "package main\n").unwrap();
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("b.go"), "package main\n").unwrap();

        let mtime = max_workspace_source_mtime(dir.path(), "go").unwrap();
        let b_mtime = std::fs::metadata(dir.path().join("b.go")).unwrap().modified().unwrap();
        assert_eq!(mtime, b_mtime,
            "max_workspace_source_mtime should return the latest mtime");
    }

    #[test]
    fn test_ensure_warm_skipped_for_unsupported_language() {
        // Languages with no warmup command must return status='skipped'
        // (graceful) instead of failing — Vestige's framing: callers
        // should be able to opt into warmup unconditionally.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "def x(): pass\n").unwrap();
        let manager = LSPManager::new(dir.path());
        let report = manager.ensure_warm("python", &src);
        assert_eq!(report.status, "skipped");
        assert!(!report.cached);
        assert_eq!(report.elapsed_ms, 0);
        assert!(report.message.is_some());
    }

    #[test]
    fn test_ensure_warm_skipped_when_no_manifest_found() {
        // CULTRA-952: supported language but no Cargo.toml between file and
        // sandbox root → skipped, with a message naming the missing manifest.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("orphan.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();
        let manager = LSPManager::new(dir.path());
        let report = manager.ensure_warm("rust", &src);
        assert_eq!(report.status, "skipped");
        assert!(report.message.unwrap().contains("Cargo.toml"));
    }

    #[test]
    fn test_ensure_warm_returns_cached_when_state_present_and_workspace_unchanged() {
        // CULTRA-950 / CULTRA-952: inject a fresh stamp via the new
        // (language, manifest_dir) cache key.
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().to_path_buf();
        std::fs::write(crate_dir.join("go.mod"), "module x\n").unwrap();
        std::fs::write(crate_dir.join("a.go"), "package main\n").unwrap();
        let src = crate_dir.join("a.go");
        let manager = LSPManager::new(dir.path());

        let future = SystemTime::now() + Duration::from_secs(3600);
        manager.inject_warmup_cache_entry("go", &crate_dir.canonicalize().unwrap(), WarmupCacheEntry {
            mtime_stamp: future,
            elapsed_ms: 1234,
            status: "warm".to_string(),
            command: Some("go build -C /x ./...".to_string()),
            message: None,
        });

        let report = manager.ensure_warm("go", &src);
        assert_eq!(report.status, "cached");
        assert!(report.cached);
        assert_eq!(report.elapsed_ms, 1234,
            "cached report should echo the elapsed_ms from the original warmup");
        assert_eq!(report.cached_status.as_deref(), Some("warm"),
            "cached_status should reflect the original outcome");
    }

    #[test]
    fn test_ensure_warm_caches_failed_warmup_too() {
        // CULTRA-952: failed warmups must also be cached so a retry storm
        // within one edit cycle doesn't replay the same multi-second failure.
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().to_path_buf();
        std::fs::write(crate_dir.join("go.mod"), "module x\n").unwrap();
        std::fs::write(crate_dir.join("a.go"), "package main\n").unwrap();
        let src = crate_dir.join("a.go");
        let manager = LSPManager::new(dir.path());

        let future = SystemTime::now() + Duration::from_secs(3600);
        manager.inject_warmup_cache_entry("go", &crate_dir.canonicalize().unwrap(), WarmupCacheEntry {
            mtime_stamp: future,
            elapsed_ms: 27_000,
            status: "failed".to_string(),
            command: Some("go build -C /x ./...".to_string()),
            message: Some("simulated 27s failure".to_string()),
        });

        let report = manager.ensure_warm("go", &src);
        assert_eq!(report.status, "cached", "cached failures must be replayed as status=cached");
        assert_eq!(report.cached_status.as_deref(), Some("failed"),
            "cached_status must indicate the original outcome was a failure");
        assert_eq!(report.elapsed_ms, 27_000);
        assert_eq!(report.message.as_deref(), Some("simulated 27s failure"));
    }

    #[test]
    fn test_ensure_warm_invalidates_when_source_modified_since_stamp() {
        // Stamp from the past → file mtime is newer → cache must invalidate.
        // The re-warm subprocess fails (no `go` toolchain in CI) but the
        // important assertion is that we did NOT hit the cache branch.
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().to_path_buf();
        std::fs::write(crate_dir.join("go.mod"), "module x\n").unwrap();
        std::fs::write(crate_dir.join("a.go"), "package main\n").unwrap();
        let src = crate_dir.join("a.go");
        let manager = LSPManager::new(dir.path());

        let past = SystemTime::UNIX_EPOCH + Duration::from_secs(1);
        manager.inject_warmup_cache_entry("go", &crate_dir.canonicalize().unwrap(), WarmupCacheEntry {
            mtime_stamp: past,
            elapsed_ms: 9999,
            status: "warm".to_string(),
            command: None,
            message: None,
        });

        let report = manager.ensure_warm("go", &src);
        assert_ne!(report.status, "cached",
            "should NOT hit cache when workspace mtime > stamp");
        assert_ne!(report.elapsed_ms, 9999,
            "should not be the cached elapsed_ms");
    }

    #[test]
    fn test_ensure_warm_separate_cache_per_crate_dir() {
        // CULTRA-952: a monorepo with two crates should have two independent
        // cache entries — warming one must not affect the other.
        let dir = tempfile::tempdir().unwrap();
        let crate_a = dir.path().join("crate_a");
        let crate_b = dir.path().join("crate_b");
        std::fs::create_dir_all(&crate_a).unwrap();
        std::fs::create_dir_all(&crate_b).unwrap();
        std::fs::write(crate_a.join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        std::fs::write(crate_b.join("Cargo.toml"), "[package]\nname=\"b\"\n").unwrap();
        let src_a = crate_a.join("lib.rs");
        let src_b = crate_b.join("lib.rs");
        std::fs::write(&src_a, "pub fn a() {}\n").unwrap();
        std::fs::write(&src_b, "pub fn b() {}\n").unwrap();

        let manager = LSPManager::new(dir.path());

        // Inject a cached "warm" for crate_a only.
        let future = SystemTime::now() + Duration::from_secs(3600);
        manager.inject_warmup_cache_entry("rust", &crate_a.canonicalize().unwrap(), WarmupCacheEntry {
            mtime_stamp: future,
            elapsed_ms: 1234,
            status: "warm".to_string(),
            command: None,
            message: None,
        });

        let a_report = manager.ensure_warm("rust", &src_a);
        assert_eq!(a_report.status, "cached", "crate_a is cached");

        let b_report = manager.ensure_warm("rust", &src_b);
        assert_ne!(b_report.status, "cached",
            "crate_b must NOT inherit crate_a's cache entry");
    }

    #[test]
    fn test_run_with_timeout_succeeds_on_quick_command() {
        // `true` exits 0 immediately on every Unix box. If your test machine
        // doesn't have it, the warmup story is doomed regardless.
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout("true", &[], dir.path(), Duration::from_secs(5));
        assert!(result.is_ok(), "expected success, got: {:?}", result);
    }

    #[test]
    fn test_run_with_timeout_reports_nonzero_exit() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout("false", &[], dir.path(), Duration::from_secs(5));
        assert!(result.is_err(), "expected failure for `false`");
        let err = result.unwrap_err();
        assert!(err.contains("false"), "error should mention the command: {}", err);
    }

    #[test]
    fn test_run_with_timeout_kills_on_timeout() {
        // `sleep 10` runs longer than the 1s timeout → must be killed.
        // Test enforces the upper bound: if the function blocks past the
        // timeout, the test framework will eventually time out, but the
        // assertion below makes the failure mode crisp.
        let dir = tempfile::tempdir().unwrap();
        let start = Instant::now();
        let result = run_with_timeout("sleep", &["10"], dir.path(), Duration::from_secs(1));
        let elapsed = start.elapsed();
        assert!(result.is_err(), "expected timeout error");
        assert!(elapsed < Duration::from_secs(3),
            "should return within ~timeout + grace period, took {:?}", elapsed);
        let err = result.unwrap_err();
        assert!(err.contains("timeout") || err.contains("exceeded"),
            "error should mention timeout: {}", err);
    }

    #[test]
    fn test_run_with_timeout_handles_missing_command() {
        let dir = tempfile::tempdir().unwrap();
        let result = run_with_timeout("this-command-does-not-exist-xyzzy", &[], dir.path(),
            Duration::from_secs(5));
        assert!(result.is_err(), "expected spawn failure");
    }
}
