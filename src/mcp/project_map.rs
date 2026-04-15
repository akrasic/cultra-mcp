//! MCP tool: `get_project_map` — workspace floorplan for boot-up orientation
//! (CULTRA-1009/1010/1011).
//!
//! Intentionally always regenerates (Path B in doc-project-map-plan) — no
//! TTL / cache-read path. Scan is cheap; simplicity beats invalidation
//! gymnastics. The `.cultra/project-map.json` artifact is written for human
//! inspection, not tool consumption.
//!
//! Extracted from `tools.rs` per CULTRA-1017 (first of a broader
//! decomposition — see that ticket).

use anyhow::{anyhow, Result};
use serde_json::{json, Map, Value};
use tracing::warn;

use crate::mcp::server::Server;

const PROJECT_MAP_DEFAULT_IGNORES: &[&str] = &[
    "node_modules", "target", "dist", "build",
    ".venv", "venv",
    ".fastembed_cache", ".next", ".svelte-kit", ".turbo",
    ".git",
    "__pycache__", ".pytest_cache", ".mypy_cache",
    "coverage", ".coverage",
];

/// scan_project_map_tool is the MCP dispatch target for `get_project_map`.
/// Resolves the optional `path` argument, enforces the workspace_root boundary,
/// and delegates to scan_project_map. Mirrors project_info_tool style.
pub(crate) fn scan_project_map_tool(args: Map<String, Value>, server: &Server) -> Result<Value> {
    let target = args
        .get("path")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| server.workspace_root.clone());

    if !target.starts_with(&server.workspace_root) {
        return Err(anyhow!("Path must be within the workspace root"));
    }
    scan_project_map(&target)
}

/// scan_project_map enumerates the workspace floorplan. Returns a Value with
/// the structure described in doc-project-map-plan. Fails-open on per-dir
/// errors — emits entries with reason fields rather than aborting the scan.
pub(crate) fn scan_project_map(workspace_root: &std::path::Path) -> Result<Value> {
    if !workspace_root.is_dir() {
        return Err(anyhow!(
            "workspace root '{}' is not a directory",
            workspace_root.display()
        ));
    }

    // Root git status: .git can be a directory (normal repo) OR a file
    // (root-as-worktree). Either way the root IS a repo.
    let root_git = workspace_root.join(".git");
    let root_is_git_repo = root_git.is_dir() || root_git.is_file();

    // Compose ignore list. Later sources union with earlier ones; we record the
    // first source that named a given dir so we can attribute "why ignored."
    let mut ignore_sources: std::collections::HashMap<String, &'static str> =
        std::collections::HashMap::new();
    for name in PROJECT_MAP_DEFAULT_IGNORES {
        ignore_sources.insert((*name).to_string(), "default");
    }
    for name in parse_ignore_file(&workspace_root.join(".gitignore")) {
        ignore_sources.entry(name).or_insert("gitignore");
    }
    for name in parse_ignore_file(&workspace_root.join(".dockerignore")) {
        ignore_sources.entry(name).or_insert("dockerignore");
    }

    let entries_iter = match std::fs::read_dir(workspace_root) {
        Ok(it) => it,
        Err(e) => return Err(anyhow!("failed to read workspace root: {}", e)),
    };

    let mut entries: Vec<Value> = Vec::new();
    let mut ignored: Vec<Value> = Vec::new();
    let mut any_nested = false;
    let mut code_entry_count = 0;

    for entry in entries_iter.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };

        // Hidden dirs are skipped unless they're a nested repo (which we always
        // surface regardless of hiding or ignore rules).
        let dotgit = path.join(".git");
        let is_own_repo = dotgit.is_dir();
        let submodule = dotgit.is_file();

        if name.starts_with('.') && !is_own_repo {
            continue;
        }

        // Ignored dirs — but nested repos always surface, ignore list be damned.
        // The boundary exists whether or not the parent tracks the contents.
        if let Some(reason) = ignore_sources.get(&name) {
            if !is_own_repo {
                ignored.push(json!({"dir": name, "reason": reason}));
                continue;
            }
        }

        if is_own_repo {
            any_nested = true;
            let has_manifest = has_any_known_manifest(&path);
            entries.push(json!({
                "dir": name,
                "is_own_repo": true,
                "submodule": false,
                "has_manifest": has_manifest,
                "notes": "separate codebase — call get_project_map with this path for details",
            }));
            continue;
        }

        // Normal or submodule: read manifest. Submodules keep full manifest info
        // because they're semantically part of the parent's history.
        match classify_entry_by_manifest(&name, &path) {
            EntryClassification::Manifest { kind, manifest, frameworks, workspace } => {
                code_entry_count += 1;
                let mut obj = serde_json::Map::new();
                obj.insert("dir".to_string(), json!(name));
                obj.insert("kind".to_string(), json!(kind));
                obj.insert("manifest".to_string(), json!(manifest));
                obj.insert("frameworks".to_string(), json!(frameworks));
                // Sparse: only emit `workspace` when true. Absence means
                // "regular crate" (the common case); presence means "Cargo workspace
                // root." Keeps response clean on repos with many non-workspace
                // crates.
                if let Some(true) = workspace {
                    obj.insert("workspace".to_string(), json!(true));
                }
                obj.insert("is_own_repo".to_string(), json!(false));
                obj.insert("submodule".to_string(), json!(submodule));
                entries.push(Value::Object(obj));
            }
            EntryClassification::Misc { reason } => {
                entries.push(json!({
                    "dir": name,
                    "kind": "misc",
                    "reason": reason,
                    "is_own_repo": false,
                    "submodule": submodule,
                }));
            }
            EntryClassification::PermissionDenied => {
                entries.push(json!({
                    "dir": name,
                    "kind": "misc",
                    "reason": "permission denied",
                    "is_own_repo": false,
                    "submodule": submodule,
                }));
            }
        }
    }

    // Determine top-level structure.
    // CULTRA-1016: an empty repo (root is a git repo, 0 code dirs, no nested)
    // is classified as "single_project" — a project with no manifests yet is
    // semantically a single project, not a "monorepo" (which means multiple
    // code dirs in one repo). Prior test hedged between the two outcomes; this
    // locks the contract.
    let structure = match (root_is_git_repo, any_nested, code_entry_count) {
        (false, false, _) => "not_a_repo",
        (true, false, c) if c <= 1 => "single_project",
        (true, false, _) => "monorepo",
        (_, true, _) => "workspace_with_nested_repos",
    };

    // CULTRA-1015: previously degraded silently to empty-string on format
    // error, which looked like a missing field to downstream consumers. Log
    // and use a recognizable sentinel so the failure is visible.
    let generated_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|e| {
            warn!(error = %e, "get_project_map: Rfc3339 format failed for generated_at");
            "unknown".to_string()
        });

    let result = json!({
        "root": workspace_root.display().to_string(),
        "root_is_git_repo": root_is_git_repo,
        "structure": structure,
        "generated_at": generated_at,
        "entries": entries,
        "ignored": ignored,
    });

    // Write the artifact for inspectability. Failures here should NOT fail the
    // scan — the client already has the result; the disk copy is a nice-to-have.
    // CULTRA-1015: previously swallowed silently via `let _ = ...`. Now logs
    // at warn-level so persistent failures (disk full, permission denied,
    // concurrent writer collision) are visible to operators.
    if let Err(e) = write_project_map_artifact(workspace_root, &result) {
        warn!(
            workspace_root = %workspace_root.display(),
            error = %e,
            "get_project_map: failed to write .cultra/project-map.json artifact"
        );
    }

    Ok(result)
}

/// merge_project_map_into runs scan_project_map on the workspace and inserts
/// the result under `project_map` in `dest` if `dest` is a JSON object.
/// Catches panics from the scanner so a buggy scan can never break session boot.
/// Existing fields in `dest` are not overwritten — the merge is purely additive.
pub(crate) fn merge_project_map_into(dest: &mut Value, workspace_root: &std::path::Path) {
    // Don't shadow an existing project_map field if the API already provided one.
    if let Some(obj) = dest.as_object() {
        if obj.contains_key("project_map") {
            return;
        }
    } else {
        return; // Non-object response: nothing to merge into.
    }

    let workspace = workspace_root.to_path_buf();
    let scan = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        scan_project_map(&workspace)
    }));

    let pm = match scan {
        Ok(Ok(v)) => v,
        Ok(Err(_)) | Err(_) => return, // scan errored or panicked — degrade silently
    };

    if let Some(obj) = dest.as_object_mut() {
        obj.insert("project_map".to_string(), pm);
    }
}

/// Entry classification result. Kept as a tagged enum so the match arm in
/// scan_project_map is exhaustive.
enum EntryClassification {
    Manifest {
        kind: String,
        manifest: String,
        frameworks: Vec<String>,
        workspace: Option<bool>,
    },
    Misc {
        reason: String,
    },
    PermissionDenied,
}

/// classify_entry_by_manifest looks for known manifest files and classifies
/// the directory's kind + frameworks. Falls back to misc with a reason string
/// when no manifest is present.
fn classify_entry_by_manifest(_name: &str, path: &std::path::Path) -> EntryClassification {
    // Manifest detection order: more specific first. pyproject beats
    // requirements.txt so that a modern Python project doesn't also get the
    // legacy indicator.
    let candidates: [(&str, &str); 6] = [
        ("Cargo.toml", "rust"),
        ("go.mod", "go"),
        ("package.json", "node"),
        ("pyproject.toml", "python"),
        ("composer.json", "php"),
        ("requirements.txt", "python"),
    ];

    for (manifest_name, kind) in &candidates {
        let manifest_path = path.join(manifest_name);
        if !manifest_path.exists() {
            continue;
        }

        let content = match std::fs::read_to_string(&manifest_path) {
            Ok(c) => c,
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                return EntryClassification::PermissionDenied;
            }
            Err(_) => {
                // File exists but we can't read it. Treat as misc rather than
                // propagating the error — the scan should always produce something.
                return EntryClassification::Misc {
                    reason: format!("manifest {} unreadable", manifest_name),
                };
            }
        };

        let mut frameworks: Vec<String> = Vec::new();
        let mut workspace: Option<bool> = None;

        match *kind {
            "node" => {
                frameworks = detect_node_frameworks(&content);
            }
            "rust" => {
                // Real workspace detection: look for [workspace] section header.
                workspace = Some(content.lines().any(|l| l.trim().starts_with("[workspace]")));
            }
            _ => {}
        }

        return EntryClassification::Manifest {
            kind: (*kind).to_string(),
            manifest: (*manifest_name).to_string(),
            frameworks,
            workspace,
        };
    }

    // No manifest — heuristic classify by extension distribution of top-level
    // non-hidden files. Crude but fast; v1 accepts the trade-off.
    let (md_ratio, txt_ratio, total) = top_level_extension_ratios(path);
    if total == 0 {
        return EntryClassification::Misc {
            reason: "no manifest".to_string(),
        };
    }
    if md_ratio > 0.7 {
        return EntryClassification::Misc {
            reason: "no manifest, mostly markdown".to_string(),
        };
    }
    if txt_ratio > 0.7 {
        return EntryClassification::Misc {
            reason: "no manifest, mostly text".to_string(),
        };
    }
    EntryClassification::Misc {
        reason: "no manifest".to_string(),
    }
}

/// detect_node_frameworks scans package.json's dependencies and devDependencies
/// for framework markers. Order within the returned vec is most-specific first
/// (e.g. next before react). See doc-project-map-plan framework table.
fn detect_node_frameworks(package_json_content: &str) -> Vec<String> {
    let parsed: Value = match serde_json::from_str(package_json_content) {
        Ok(v) => v,
        Err(_) => return Vec::new(),
    };

    // Gather all dep keys from both dependencies and devDependencies.
    let mut deps: std::collections::HashSet<String> = std::collections::HashSet::new();
    for field in &["dependencies", "devDependencies"] {
        if let Some(obj) = parsed.get(field).and_then(|v| v.as_object()) {
            for k in obj.keys() {
                deps.insert(k.clone());
            }
        }
    }

    let mut frameworks: Vec<String> = Vec::new();

    // Most-specific first. Next includes react because Next IS a React framework.
    if deps.contains("next") {
        frameworks.push("next".to_string());
        frameworks.push("react".to_string());
    } else if deps.contains("react") {
        frameworks.push("react".to_string());
    }

    if deps.contains("svelte") || deps.contains("@sveltejs/kit") {
        frameworks.push("svelte".to_string());
    }
    if deps.contains("vue") || deps.iter().any(|d| d.starts_with("@vue/")) {
        frameworks.push("vue".to_string());
    }
    if deps.contains("@angular/core") {
        frameworks.push("angular".to_string());
    }
    if deps.contains("astro") {
        frameworks.push("astro".to_string());
    }

    frameworks
}

/// top_level_extension_ratios returns (markdown_ratio, text_ratio, total_files).
/// Cheap heuristic over direct (non-recursive) children. Used only when no
/// manifest file exists to give a "misc" directory a more useful reason.
fn top_level_extension_ratios(dir: &std::path::Path) -> (f32, f32, usize) {
    let mut md = 0usize;
    let mut txt = 0usize;
    let mut total = 0usize;

    let iter = match std::fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return (0.0, 0.0, 0),
    };

    for entry in iter.flatten().take(50) {
        let p = entry.path();
        if !p.is_file() {
            continue;
        }
        let name = match p.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if name.starts_with('.') {
            continue;
        }
        total += 1;
        let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("");
        match ext {
            "md" | "mdx" => md += 1,
            "txt" | "rst" | "adoc" => txt += 1,
            _ => {}
        }
    }

    if total == 0 {
        return (0.0, 0.0, 0);
    }
    (md as f32 / total as f32, txt as f32 / total as f32, total)
}

/// parse_ignore_file reads a .gitignore or .dockerignore and returns the
/// top-level directory names that would be ignored. Line-by-line dumb match
/// — no !negations, no recursive **, no full gitignore semantics (v1).
fn parse_ignore_file(path: &std::path::Path) -> Vec<String> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let mut names = Vec::new();
    for raw in content.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
            continue;
        }
        // Strip leading/trailing slashes and any trailing glob.
        let stripped = line.trim_start_matches('/').trim_end_matches('/');
        // Only consider simple top-level names. Anything with path separators,
        // globs, or negations is outside v1 scope.
        if stripped.contains('/') || stripped.contains('*') || stripped.contains('?') {
            continue;
        }
        if !stripped.is_empty() {
            names.push(stripped.to_string());
        }
    }
    names
}

/// has_any_known_manifest returns true if the directory has any file that
/// would identify it as a code project. Used for the pruned nested-repo entry
/// to tell agents "yes, you can call get_project_map here to learn more"
/// without actually reading anything across the repo boundary.
fn has_any_known_manifest(dir: &std::path::Path) -> bool {
    for name in &["Cargo.toml", "go.mod", "package.json", "pyproject.toml", "composer.json", "requirements.txt"] {
        if dir.join(name).exists() {
            return true;
        }
    }
    false
}

/// write_project_map_artifact writes the scan result to .cultra/project-map.json
/// via atomic rename (tmp + rename). CULTRA-1015 tracks improving the caller
/// to log at warn-level when this fails.
fn write_project_map_artifact(workspace_root: &std::path::Path, value: &Value) -> Result<()> {
    let cultra_dir = workspace_root.join(".cultra");
    if !cultra_dir.exists() {
        std::fs::create_dir_all(&cultra_dir)?;
    }
    let final_path = cultra_dir.join("project-map.json");
    let tmp_path = cultra_dir.join("project-map.json.tmp");
    let content = serde_json::to_string_pretty(value)?;
    std::fs::write(&tmp_path, content)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mcp::test_helpers::test_server;

    // fixture: empty repo root (has .git but no top-level code dirs)
    fn mk_empty_repo(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("empty");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        root
    }

    // fixture: monorepo with go + rust + node dirs, root .git only
    fn mk_monorepo(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("mono");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("svc")).unwrap();
        std::fs::write(root.join("svc").join("go.mod"), "module foo\n\ngo 1.22\n").unwrap();
        std::fs::create_dir_all(root.join("engine")).unwrap();
        std::fs::write(root.join("engine").join("Cargo.toml"), "[package]\nname=\"e\"\nversion=\"0.1\"\n").unwrap();
        std::fs::create_dir_all(root.join("web")).unwrap();
        std::fs::write(
            root.join("web").join("package.json"),
            r#"{"name":"web","dependencies":{"react":"18.0.0","next":"14.0.0"}}"#,
        ).unwrap();
        root
    }

    // fixture: workspace containing a nested free-standing repo
    fn mk_workspace_with_nested(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("ws");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("lib")).unwrap();
        std::fs::write(root.join("lib").join("go.mod"), "module lib\n").unwrap();
        // nested free-standing repo
        std::fs::create_dir_all(root.join("vendor-tool").join(".git")).unwrap();
        std::fs::write(
            root.join("vendor-tool").join("Cargo.toml"),
            "[package]\nname=\"v\"\n",
        ).unwrap();
        root
    }

    // fixture: submodule (.git as FILE, not dir)
    fn mk_submodule(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("sub");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("submod")).unwrap();
        // gitlink file: contents mimic a real submodule pointer
        std::fs::write(
            root.join("submod").join(".git"),
            "gitdir: ../.git/modules/submod\n",
        ).unwrap();
        std::fs::write(root.join("submod").join("go.mod"), "module submod\n").unwrap();
        root
    }

    // fixture: no-git workspace — just a container with one code dir
    fn mk_no_git(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("nogit");
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(
            root.join("app").join("package.json"),
            r#"{"name":"app","dependencies":{"svelte":"4.0.0"}}"#,
        ).unwrap();
        root
    }

    // fixture: mostly-markdown dir, no manifest
    fn mk_mostly_markdown(tmp: &std::path::Path) -> std::path::PathBuf {
        let root = tmp.join("mddir");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("docs")).unwrap();
        for i in 0..10 {
            std::fs::write(
                root.join("docs").join(format!("f{}.md", i)),
                "# content",
            ).unwrap();
        }
        root
    }

    #[test]
    fn test_project_map_empty_repo_is_single_project() {
        // CULTRA-1016: contract locked. An empty git repo is "single_project"
        // (no manifests yet, but semantically a project — not a monorepo).
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_empty_repo(tmp.path());
        let result = scan_project_map(&root).unwrap();
        assert_eq!(result["root_is_git_repo"], json!(true));
        assert_eq!(
            result["structure"], json!("single_project"),
            "empty repo must be single_project, not monorepo"
        );
        assert_eq!(result["entries"].as_array().unwrap().len(), 0);
    }

    #[test]
    fn test_project_map_monorepo_has_structure_monorepo() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let result = scan_project_map(&root).unwrap();
        assert_eq!(result["structure"], json!("monorepo"));
        assert_eq!(result["root_is_git_repo"], json!(true));
        let entries = result["entries"].as_array().unwrap();
        assert_eq!(entries.len(), 3, "expected 3 code dirs, got {:?}", entries);
        let dirs: Vec<&str> = entries.iter().map(|e| e["dir"].as_str().unwrap()).collect();
        assert!(dirs.contains(&"svc"));
        assert!(dirs.contains(&"engine"));
        assert!(dirs.contains(&"web"));
    }

    #[test]
    fn test_project_map_nested_repo_without_manifest_still_pruned() {
        // CULTRA-1016: the has_manifest: false branch was previously untested.
        // A nested repo with no recognized manifest (e.g. a vendored tool
        // that uses plain scripts + make) must still appear as a pruned entry,
        // flagged is_own_repo: true but has_manifest: false.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("nested-no-mf");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("vendor-scripts").join(".git")).unwrap();
        // vendor-scripts has no Cargo.toml / package.json / go.mod / etc.
        std::fs::write(root.join("vendor-scripts").join("build.sh"), "#!/bin/sh\n").unwrap();

        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let nested = entries.iter().find(|e| e["dir"] == json!("vendor-scripts")).unwrap();
        assert_eq!(nested["is_own_repo"], json!(true));
        assert_eq!(nested["has_manifest"], json!(false),
            "nested repo without recognized manifest must report has_manifest: false");
        // Pruned shape: no manifest details even though has_manifest is false.
        assert!(nested.get("kind").is_none());
        assert!(nested.get("manifest").is_none());
        assert!(nested.get("frameworks").is_none());
    }

    #[test]
    fn test_project_map_nested_repo_is_pruned() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_workspace_with_nested(tmp.path());
        let result = scan_project_map(&root).unwrap();
        assert_eq!(result["structure"], json!("workspace_with_nested_repos"));
        let entries = result["entries"].as_array().unwrap();
        let nested = entries.iter().find(|e| e["dir"] == json!("vendor-tool")).unwrap();
        assert_eq!(nested["is_own_repo"], json!(true));
        assert_eq!(nested["has_manifest"], json!(true));
        // Pruned entry must not leak manifest details.
        assert!(nested.get("kind").is_none(), "nested repo entry must not expose kind");
        assert!(nested.get("frameworks").is_none(), "nested repo entry must not expose frameworks");
        assert!(nested.get("manifest").is_none(), "nested repo entry must not expose manifest");
    }

    #[test]
    fn test_project_map_submodule_keeps_full_manifest_but_flags_true() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_submodule(tmp.path());
        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let sub = entries.iter().find(|e| e["dir"] == json!("submod")).unwrap();
        assert_eq!(sub["is_own_repo"], json!(false));
        assert_eq!(sub["submodule"], json!(true));
        // Submodule keeps full manifest info (semantically part of parent).
        assert_eq!(sub["kind"], json!("go"));
        assert_eq!(sub["manifest"], json!("go.mod"));
    }

    #[test]
    fn test_project_map_no_git_workspace() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_no_git(tmp.path());
        let result = scan_project_map(&root).unwrap();
        assert_eq!(result["root_is_git_repo"], json!(false));
        assert_eq!(result["structure"], json!("not_a_repo"));
        let entries = result["entries"].as_array().unwrap();
        let app = entries.iter().find(|e| e["dir"] == json!("app")).unwrap();
        assert_eq!(app["kind"], json!("node"));
        let fw = app["frameworks"].as_array().unwrap();
        assert!(fw.iter().any(|f| f == &json!("svelte")));
    }

    #[test]
    fn test_project_map_react_next_frameworks_detected() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let web = entries.iter().find(|e| e["dir"] == json!("web")).unwrap();
        assert_eq!(web["kind"], json!("node"));
        let fw: Vec<&str> = web["frameworks"].as_array().unwrap()
            .iter().map(|v| v.as_str().unwrap()).collect();
        // Most-specific first: next added before react.
        assert_eq!(fw[0], "next", "frameworks[0] must be most-specific (next)");
        assert!(fw.contains(&"react"), "Next implies React");
    }

    #[test]
    fn test_project_map_misc_reason_for_mostly_markdown() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_mostly_markdown(tmp.path());
        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let docs = entries.iter().find(|e| e["dir"] == json!("docs")).unwrap();
        assert_eq!(docs["kind"], json!("misc"));
        assert!(docs["reason"].as_str().unwrap().contains("markdown"));
    }

    #[test]
    fn test_project_map_gitignored_dir_surfaces_in_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("gi");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("tmp-stuff")).unwrap();
        std::fs::create_dir_all(root.join("real-code")).unwrap();
        std::fs::write(root.join("real-code").join("go.mod"), "module r\n").unwrap();
        std::fs::write(root.join(".gitignore"), "tmp-stuff\n").unwrap();

        let result = scan_project_map(&root).unwrap();
        let ignored = result["ignored"].as_array().unwrap();
        let hit = ignored.iter().find(|e| e["dir"] == json!("tmp-stuff")).unwrap();
        assert_eq!(hit["reason"], json!("gitignore"));
        // real-code still appears in entries
        let entries = result["entries"].as_array().unwrap();
        assert!(entries.iter().any(|e| e["dir"] == json!("real-code")));
    }

    #[test]
    fn test_project_map_cache_artifact_written() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let _ = scan_project_map(&root).unwrap();
        let artifact = root.join(".cultra").join("project-map.json");
        assert!(artifact.exists(), "cache artifact should be written to .cultra/project-map.json");
        let content = std::fs::read_to_string(&artifact).unwrap();
        let parsed: Value = serde_json::from_str(&content).unwrap();
        assert_eq!(parsed["structure"], json!("monorepo"));
    }

    #[test]
    fn test_project_map_single_project_structure() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("sp");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("app")).unwrap();
        std::fs::write(root.join("app").join("Cargo.toml"), "[package]\nname=\"a\"\n").unwrap();
        let result = scan_project_map(&root).unwrap();
        assert_eq!(result["structure"], json!("single_project"));
    }

    #[test]
    fn test_project_map_cargo_workspace_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cw");
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("ws-pkg")).unwrap();
        std::fs::write(
            root.join("ws-pkg").join("Cargo.toml"),
            "[workspace]\nmembers = [\"a\"]\n",
        ).unwrap();
        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let pkg = entries.iter().find(|e| e["dir"] == json!("ws-pkg")).unwrap();
        assert_eq!(pkg["workspace"], json!(true));
    }

    #[test]
    fn test_project_map_workspace_absent_on_regular_crate() {
        // Sparse-emission contract: non-workspace Rust entries must NOT include
        // a `workspace` field. Absence means "regular crate."
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path()); // engine/Cargo.toml has no [workspace]
        let result = scan_project_map(&root).unwrap();
        let entries = result["entries"].as_array().unwrap();
        let engine = entries.iter().find(|e| e["dir"] == json!("engine")).unwrap();
        assert_eq!(engine["kind"], json!("rust"));
        assert!(engine.get("workspace").is_none(),
            "regular Rust crate must not carry a workspace field; got {:?}", engine);
    }

    /// Ad-hoc "what does the real Cultra workspace look like?" dump.
    /// Ignored by default so CI doesn't depend on filesystem layout; run with
    /// `cargo test --release dump_real_workspace -- --ignored --nocapture`.
    #[test]
    #[ignore]
    fn dump_real_workspace_project_map() {
        let here = std::env::var("CARGO_MANIFEST_DIR").unwrap();
        let workspace_root = std::path::PathBuf::from(&here).parent().unwrap().to_path_buf();
        let result = scan_project_map(&workspace_root).expect("scan should succeed");
        println!("{}", serde_json::to_string_pretty(&result).unwrap());
    }

    #[test]
    fn test_project_map_rejects_path_outside_workspace() {
        // scan_project_map_tool enforces the boundary; make a server with
        // a narrow workspace root and try to read outside it.
        let tmp = tempfile::tempdir().unwrap();
        let inside = tmp.path().join("inside");
        std::fs::create_dir_all(&inside).unwrap();
        let outside = tmp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();

        let mut server = test_server();
        server.workspace_root = inside.clone();
        let mut args = Map::new();
        args.insert("path".to_string(), json!(outside.to_string_lossy()));
        let err = scan_project_map_tool(args, &server).unwrap_err();
        assert!(err.to_string().contains("workspace root"),
            "expected boundary error, got: {}", err);
    }

    #[test]
    fn test_get_project_map_defaults_to_workspace_root() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let mut server = test_server();
        server.workspace_root = root.clone();

        let result = scan_project_map_tool(Map::new(), &server).unwrap();
        assert_eq!(result["root"], json!(root.display().to_string()));
        assert_eq!(result["structure"], json!("monorepo"));
    }

    #[test]
    fn test_get_project_map_returns_expected_top_level_keys() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let mut server = test_server();
        server.workspace_root = root.clone();

        let result = scan_project_map_tool(Map::new(), &server).unwrap();
        let obj = result.as_object().expect("top-level must be object");
        for key in &["root", "root_is_git_repo", "structure", "generated_at", "entries", "ignored"] {
            assert!(obj.contains_key(*key), "missing top-level key: {}", key);
        }
        assert!(result["entries"].is_array());
        assert!(result["ignored"].is_array());
    }

    // ------------------------------------------------------------------
    // CULTRA-1011: load_session_state merge helper tests
    // ------------------------------------------------------------------

    #[test]
    fn test_merge_project_map_inserts_field_on_object_response() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());

        let mut response = json!({
            "found": true,
            "session": {"session_id": "abc", "project_id": "proj-x"},
        });
        merge_project_map_into(&mut response, &root);

        // Existing fields preserved.
        assert_eq!(response["found"], json!(true));
        assert_eq!(response["session"]["session_id"], json!("abc"));
        // project_map merged.
        assert!(response["project_map"].is_object());
        assert_eq!(response["project_map"]["structure"], json!("monorepo"));
    }

    #[test]
    fn test_merge_project_map_does_not_overwrite_existing_field() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());

        let original = json!({"already_there": "do not touch me"});
        let mut response = json!({
            "session": {"session_id": "abc"},
            "project_map": original.clone(),
        });
        merge_project_map_into(&mut response, &root);

        assert_eq!(response["project_map"], original,
            "merge must not overwrite a project_map field already present in the response");
    }

    #[test]
    fn test_merge_project_map_degrades_gracefully_on_scan_error() {
        let nonexistent = std::path::PathBuf::from("/this/path/should/not/exist/anywhere");
        let mut response = json!({
            "found": true,
            "session": {"session_id": "abc"},
        });
        merge_project_map_into(&mut response, &nonexistent);

        assert_eq!(response["found"], json!(true));
        assert_eq!(response["session"]["session_id"], json!("abc"));
        assert!(response.get("project_map").is_none(),
            "scan failure must not insert a project_map field");
    }

    #[test]
    fn test_merge_project_map_skips_non_object_response() {
        let tmp = tempfile::tempdir().unwrap();
        let root = mk_monorepo(tmp.path());
        let mut response = json!([1, 2, 3]);
        merge_project_map_into(&mut response, &root);
        assert_eq!(response, json!([1, 2, 3]));
    }
}
