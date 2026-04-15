// CULTRA-952 / CULTRA-954 / CULTRA-955: shared workspace-root resolver.
//
// Walks up from a source file looking for an "anchor" — a Cargo.toml,
// go.mod, tsconfig.json, .git directory, etc. — and returns the directory
// containing it. Bounded by the MCP sandbox root: the walk-up cannot
// escape the sandbox even when an anchor exists above it.
//
// This is the shared fix for the family of bugs where MCP tools assumed
// `cwd == workspace root`. CULTRA-952 first hit this in the warmup path;
// CULTRA-954 surfaces it in `diff_file_ast` (git root); CULTRA-955 surfaces
// it in three LSP-backed tools (workspace root for rust-analyzer / gopls /
// pyright / tsserver). Same shape, four call sites — one helper.

use anyhow::{anyhow, Result};
use std::path::{Path, PathBuf};

/// What the resolver walks up looking for.
///
/// `Manifest` is for "directory containing one of these filenames" — the
/// common case for language workspace roots. Multiple filenames means
/// "first match wins" in priority order, e.g. tsconfig.json before
/// package.json for TS/JS.
///
/// `Marker` is for "directory containing this entry, file or directory" —
/// used by `.git` (which is a directory in normal repos and a file in git
/// worktrees, both of which mark the repo root).
#[derive(Debug, Clone)]
pub enum WorkspaceAnchor {
    Manifest(&'static [&'static str]),
    Marker(&'static str),
}

/// A resolved workspace root: the directory containing the anchor and
/// the absolute path of the anchor itself. Both are canonicalized so
/// callers can compare them against other canonicalized paths without
/// symlink mismatches.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRoot {
    /// Canonicalized directory containing the anchor.
    pub root: PathBuf,
    /// Canonicalized absolute path of the anchor itself (Cargo.toml,
    /// .git, etc).
    pub anchor: PathBuf,
}

impl WorkspaceRoot {
    /// Compute the path of `file` relative to `self.root`. Used by tools
    /// that need to construct repo-relative paths (e.g. `git show
    /// HEAD:<rel_path>`).
    pub fn relative_path<'a>(&self, file: &'a Path) -> Result<&'a Path> {
        file.strip_prefix(&self.root).map_err(|_| {
            anyhow!(
                "file '{}' is not within workspace root '{}'",
                file.display(),
                self.root.display()
            )
        })
    }
}

/// Walk up from `file_path`'s parent directory looking for `anchor`. Stops
/// at `sandbox_root` (cannot escape it) and returns None if no anchor is
/// found between `file_path` and the sandbox root.
///
/// Both `file_path` and `sandbox_root` are canonicalized first, and the
/// resolver refuses files outside the sandbox even if such files exist on
/// disk. Belt-and-braces parent check on every walk-up step ensures the
/// search cannot accidentally escape via symlinks.
///
/// This is a pure filesystem-read operation — no subprocess, no cwd
/// change, nothing that touches the sandbox enforcement layer.
pub fn resolve_workspace_root(
    file_path: &Path,
    anchor: &WorkspaceAnchor,
    sandbox_root: &Path,
) -> Option<WorkspaceRoot> {
    let canonical_sandbox = sandbox_root.canonicalize().ok()?;
    let canonical_file = file_path.canonicalize().ok()?;
    if !canonical_file.starts_with(&canonical_sandbox) {
        return None;
    }

    let mut dir = canonical_file.parent()?.to_path_buf();
    loop {
        if let Some(found) = anchor_in(&dir, anchor) {
            return Some(WorkspaceRoot {
                root: dir,
                anchor: found,
            });
        }
        if dir == canonical_sandbox {
            return None;
        }
        match dir.parent() {
            Some(parent) => {
                if !parent.starts_with(&canonical_sandbox) {
                    return None;
                }
                dir = parent.to_path_buf();
            }
            None => return None,
        }
    }
}

/// Test the directory `dir` for the presence of `anchor`. Returns the
/// absolute path of the matched anchor, or None if no match.
fn anchor_in(dir: &Path, anchor: &WorkspaceAnchor) -> Option<PathBuf> {
    match anchor {
        WorkspaceAnchor::Manifest(names) => {
            for name in *names {
                let candidate = dir.join(name);
                if candidate.is_file() {
                    return Some(candidate);
                }
            }
            None
        }
        WorkspaceAnchor::Marker(name) => {
            let candidate = dir.join(name);
            // .git is a directory in normal repos, a file in git worktrees,
            // and a symlink sometimes. Accept any of those.
            if candidate.exists() {
                Some(candidate)
            } else {
                None
            }
        }
    }
}

// ============================================================================
// Convenience wrappers for the common anchor types.
// ============================================================================

/// `.git` repo root for `file_path`. Used by CULTRA-954 (`diff_file_ast`)
/// and any future git-based tool.
pub fn git_repo_root(file_path: &Path, sandbox_root: &Path) -> Option<WorkspaceRoot> {
    static GIT_ANCHOR: WorkspaceAnchor = WorkspaceAnchor::Marker(".git");
    resolve_workspace_root(file_path, &GIT_ANCHOR, sandbox_root)
}

/// LSP workspace root for the given language, used to point rust-analyzer /
/// gopls / pyright / tsserver at the right project. Returns None for
/// languages with no manifest convention. Used by CULTRA-955 (LSP shim
/// tools) and reused internally by the warmup path.
///
/// Anchor priority per language matches what the LSP server itself would
/// pick if it walked up from the file: Cargo.toml for Rust, go.mod for Go,
/// tsconfig.json before package.json for TS/JS (TS-aware servers prefer
/// the explicit project file).
pub fn lsp_workspace_root_for_language(
    language: &str,
    file_path: &Path,
    sandbox_root: &Path,
) -> Option<WorkspaceRoot> {
    static RUST: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["Cargo.toml"]);
    static GO: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["go.mod"]);
    static TS_JS: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["tsconfig.json", "package.json"]);
    static PY: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["pyproject.toml", "setup.py", "setup.cfg"]);
    // CULTRA-972: Svelte projects anchor on svelte.config.js first (project root),
    // with package.json as fallback for simpler setups.
    static SVELTE: WorkspaceAnchor = WorkspaceAnchor::Manifest(&["svelte.config.js", "package.json"]);

    let anchor: &WorkspaceAnchor = match language {
        "rust" => &RUST,
        "go" => &GO,
        "typescript" | "tsx" | "javascript" | "jsx" => &TS_JS,
        "python" => &PY,
        "svelte" => &SVELTE,
        _ => return None,
    };
    resolve_workspace_root(file_path, anchor, sandbox_root)
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolve_finds_manifest_in_same_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let src = dir.path().join("main.rs");
        std::fs::write(&src, "fn main() {}\n").unwrap();

        let result = lsp_workspace_root_for_language("rust", &src, dir.path()).unwrap();
        assert_eq!(result.root, dir.path().canonicalize().unwrap());
        assert!(result.anchor.ends_with("Cargo.toml"));
    }

    #[test]
    fn test_resolve_walks_up_through_multiple_levels() {
        let dir = tempfile::tempdir().unwrap();
        let crate_dir = dir.path().join("vux");
        let nested = crate_dir.join("src").join("foo").join("bar");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(crate_dir.join("Cargo.toml"), "[package]\nname=\"vux\"\n").unwrap();
        let src = nested.join("deep.rs");
        std::fs::write(&src, "pub fn x() {}\n").unwrap();

        let result = lsp_workspace_root_for_language("rust", &src, dir.path()).unwrap();
        assert_eq!(result.root, crate_dir.canonicalize().unwrap());
    }

    #[test]
    fn test_resolve_returns_none_when_no_anchor() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("orphan.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();
        assert!(lsp_workspace_root_for_language("rust", &src, dir.path()).is_none());
    }

    #[test]
    fn test_resolve_does_not_escape_sandbox_via_manifest() {
        // Cargo.toml exists ABOVE the sandbox; the walk-up must stop at
        // the sandbox boundary even though a valid anchor exists above it.
        let outer = tempfile::tempdir().unwrap();
        let sandbox = outer.path().join("sandbox");
        let src_dir = sandbox.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(outer.path().join("Cargo.toml"), "[package]\nname=\"escape\"\n").unwrap();
        let src = src_dir.join("a.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();

        assert!(lsp_workspace_root_for_language("rust", &src, &sandbox).is_none());
    }

    #[test]
    fn test_resolve_does_not_escape_sandbox_via_git() {
        // .git above sandbox must NOT be discoverable.
        let outer = tempfile::tempdir().unwrap();
        let sandbox = outer.path().join("sandbox");
        let src_dir = sandbox.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(outer.path().join(".git")).unwrap();
        let src = src_dir.join("a.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();

        assert!(git_repo_root(&src, &sandbox).is_none());
    }

    #[test]
    fn test_git_repo_root_finds_dot_git_dir() {
        // CULTRA-954 reproduction: sandbox at /vestige, git repo at
        // /vestige/vux/.git, source at /vestige/vux/src/compositor.rs.
        let outer = tempfile::tempdir().unwrap();
        let crate_dir = outer.path().join("vux");
        let src_dir = crate_dir.join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::create_dir_all(crate_dir.join(".git")).unwrap();
        let src = src_dir.join("compositor.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();

        let result = git_repo_root(&src, outer.path()).unwrap();
        assert_eq!(result.root, crate_dir.canonicalize().unwrap());
        assert!(result.anchor.ends_with(".git"));
    }

    #[test]
    fn test_git_repo_root_accepts_dot_git_as_file() {
        // Git worktrees use `.git` as a file (containing `gitdir: ...`)
        // rather than a directory. Anchor::Marker accepts any entry that
        // exists, so this should resolve correctly.
        let outer = tempfile::tempdir().unwrap();
        let worktree = outer.path().join("worktree");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: /elsewhere/.git/worktrees/x\n").unwrap();
        let src = worktree.join("a.rs");
        std::fs::write(&src, "fn x() {}\n").unwrap();

        let result = git_repo_root(&src, outer.path()).unwrap();
        assert_eq!(result.root, worktree.canonicalize().unwrap());
    }

    #[test]
    fn test_lsp_workspace_root_typescript_prefers_tsconfig() {
        // tsconfig.json has priority over package.json for TS projects —
        // the TS-aware LSPs all prefer the explicit project file.
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let src = dir.path().join("a.ts");
        std::fs::write(&src, "export {};\n").unwrap();

        let result = lsp_workspace_root_for_language("typescript", &src, dir.path()).unwrap();
        assert!(result.anchor.ends_with("tsconfig.json"),
            "tsconfig.json should take priority over package.json");
    }

    #[test]
    fn test_lsp_workspace_root_typescript_falls_back_to_package_json() {
        // No tsconfig.json → fall back to package.json (still a valid LSP
        // project marker for JS-only projects).
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let src = dir.path().join("a.js");
        std::fs::write(&src, "export {};\n").unwrap();

        let result = lsp_workspace_root_for_language("javascript", &src, dir.path()).unwrap();
        assert!(result.anchor.ends_with("package.json"));
    }

    #[test]
    fn test_lsp_workspace_root_python() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\nname=\"x\"\n").unwrap();
        let src = dir.path().join("a.py");
        std::fs::write(&src, "def x(): pass\n").unwrap();

        let result = lsp_workspace_root_for_language("python", &src, dir.path()).unwrap();
        assert!(result.anchor.ends_with("pyproject.toml"));
    }

    #[test]
    fn test_lsp_workspace_root_unsupported_language() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("a.rb");
        std::fs::write(&src, "def x; end\n").unwrap();
        assert!(lsp_workspace_root_for_language("ruby", &src, dir.path()).is_none());
    }

    #[test]
    fn test_relative_path_method() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname=\"x\"\n").unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir(&src_dir).unwrap();
        let src = src_dir.join("lib.rs");
        std::fs::write(&src, "pub fn x() {}\n").unwrap();

        let workspace = lsp_workspace_root_for_language("rust", &src, dir.path()).unwrap();
        let canonical_src = src.canonicalize().unwrap();
        let rel = workspace.relative_path(&canonical_src).unwrap();
        assert_eq!(rel, Path::new("src/lib.rs"));
    }
}
