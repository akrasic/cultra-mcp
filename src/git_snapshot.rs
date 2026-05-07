//! Frozen point-in-time snapshot of a git working tree, for inclusion on
//! `session_state` records (CULTRA-1079).
//!
//! Capture is best-effort: any failure returns `None`. The snapshot must
//! never fail the enclosing `save_session_state` / `load_session_state`
//! call — it is metadata, not a precondition.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use std::path::Path;
use std::process::Command;
use tracing::debug;

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone)]
pub struct GitSnapshot {
    pub branch: String,
    pub last_commit_sha: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub upstream: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ahead: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub behind: Option<i64>,
    pub dirty: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dirty_summary: Option<DirtySummary>,
    pub captured_at: String,
}

#[derive(Debug, Serialize, Deserialize, PartialEq, Clone, Default)]
pub struct DirtySummary {
    pub modified: u32,
    pub added: u32,
    pub deleted: u32,
    pub renamed: u32,
    pub untracked: u32,
}

/// Capture the working-tree snapshot for the git repo containing
/// `workspace_root`. Returns `None` for non-repos, missing `git` binary,
/// non-zero exit, malformed output, or non-UTF8 stdout.
pub fn capture(workspace_root: &Path) -> Option<GitSnapshot> {
    let output = Command::new("git")
        .args(["status", "--porcelain=v2", "--branch"])
        .current_dir(workspace_root)
        .output()
        .ok()?;
    if !output.status.success() {
        debug!(
            workspace_root = %workspace_root.display(),
            "git_snapshot: git status exited non-zero"
        );
        return None;
    }
    let stdout = String::from_utf8(output.stdout).ok()?;
    parse_porcelain_v2(&stdout)
}

fn parse_porcelain_v2(stdout: &str) -> Option<GitSnapshot> {
    let mut branch: Option<String> = None;
    let mut last_commit_sha: Option<String> = None;
    let mut upstream: Option<String> = None;
    let mut ahead: Option<i64> = None;
    let mut behind: Option<i64> = None;
    let mut summary = DirtySummary::default();

    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("# ") {
            if let Some(sha) = rest.strip_prefix("branch.oid ") {
                last_commit_sha = Some(sha.to_string());
            } else if let Some(name) = rest.strip_prefix("branch.head ") {
                branch = Some(name.to_string());
            } else if let Some(up) = rest.strip_prefix("branch.upstream ") {
                upstream = Some(up.to_string());
            } else if let Some(ab) = rest.strip_prefix("branch.ab ") {
                let mut parts = ab.split_whitespace();
                if let (Some(a), Some(b)) = (parts.next(), parts.next()) {
                    ahead = a.strip_prefix('+').and_then(|s| s.parse::<i64>().ok());
                    behind = b.strip_prefix('-').and_then(|s| s.parse::<i64>().ok());
                }
            }
        } else if let Some(rest) = line.strip_prefix("1 ") {
            // ordinary changed entry: "<XY> <SUB> <modes> <hashes> <PATH>"
            // XY is exactly 2 chars; X = staged status, Y = unstaged status.
            // A single line covers both staging sides — count once per bucket.
            let xy = rest.get(..2).unwrap_or("");
            if xy.contains('M') {
                summary.modified += 1;
            }
            if xy.contains('A') {
                summary.added += 1;
            }
            if xy.contains('D') {
                summary.deleted += 1;
            }
        } else if line.starts_with("2 ") {
            summary.renamed += 1;
        } else if line.starts_with("? ") {
            summary.untracked += 1;
        }
        // `u ` (unmerged) and `! ` (ignored) lines: ignored in v1.
    }

    let branch = branch?;
    let last_commit_sha = last_commit_sha?;
    let total =
        summary.modified + summary.added + summary.deleted + summary.renamed + summary.untracked;
    let dirty = total > 0;
    let dirty_summary = if dirty { Some(summary) } else { None };

    let captured_at = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_else(|_| "unknown".to_string());

    Some(GitSnapshot {
        branch,
        last_commit_sha,
        upstream,
        ahead,
        behind,
        dirty,
        dirty_summary,
        captured_at,
    })
}

/// CULTRA-1080: best-effort inject of `git_snapshot` into
/// `args.context_snapshot` for `save_session_state`. Capture failures are
/// silent — the snapshot is metadata, not a precondition.
pub(crate) fn inject_into_save_args(args: &mut Map<String, Value>, workspace_root: &Path) {
    if let Some(snap) = capture(workspace_root) {
        inject_snapshot(args, &snap);
    }
}

/// Pure helper: insert a captured snapshot into
/// `args.context_snapshot.git_snapshot`. No-op if `context_snapshot` is
/// missing or not an object — we don't synthesize one just to hold the
/// snapshot (it would bypass Engine V3 validation).
fn inject_snapshot(args: &mut Map<String, Value>, snap: &GitSnapshot) {
    let snap_json = match serde_json::to_value(snap) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(cs) = args
        .get_mut("context_snapshot")
        .and_then(|v| v.as_object_mut())
    {
        cs.insert("git_snapshot".to_string(), snap_json);
    }
}

/// CULTRA-1081: best-effort merge of a fresh `current_git` snapshot into a
/// `load_session_state` response. Mirrors the shape of
/// `merge_project_map_into` (project_map.rs).
pub(crate) fn merge_current_git_into(dest: &mut Value, workspace_root: &Path) {
    if let Some(snap) = capture(workspace_root) {
        merge_current_git(dest, &snap);
    }
}

/// Pure helper: merge a captured snapshot under `current_git` in `dest`.
/// No-op if `dest` is not an object, or already has a `current_git` key
/// (don't shadow an explicit value from the API).
fn merge_current_git(dest: &mut Value, snap: &GitSnapshot) {
    match dest.as_object() {
        Some(obj) if obj.contains_key("current_git") => return,
        Some(_) => {}
        None => return,
    }
    let snap_json = match serde_json::to_value(snap) {
        Ok(v) => v,
        Err(_) => return,
    };
    if let Some(obj) = dest.as_object_mut() {
        obj.insert("current_git".to_string(), snap_json);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(s: &str) -> GitSnapshot {
        parse_porcelain_v2(s).expect("parser should succeed for valid input")
    }

    fn fake_snapshot() -> GitSnapshot {
        GitSnapshot {
            branch: "main".to_string(),
            last_commit_sha: "deadbeef".to_string(),
            upstream: Some("origin/main".to_string()),
            ahead: Some(0),
            behind: Some(0),
            dirty: false,
            dirty_summary: None,
            captured_at: "2026-05-05T16:42:00Z".to_string(),
        }
    }

    #[test]
    fn clean_repo() {
        let out = "\
# branch.oid ffe1ebc5c6f56196a11698aef8bfa4ce8b45646a
# branch.head main
# branch.upstream origin/main
# branch.ab +0 -0
";
        let snap = parse(out);
        assert_eq!(snap.branch, "main");
        assert_eq!(
            snap.last_commit_sha,
            "ffe1ebc5c6f56196a11698aef8bfa4ce8b45646a"
        );
        assert_eq!(snap.upstream.as_deref(), Some("origin/main"));
        assert_eq!(snap.ahead, Some(0));
        assert_eq!(snap.behind, Some(0));
        assert!(!snap.dirty);
        assert!(snap.dirty_summary.is_none());
    }

    #[test]
    fn dirty_modified_only() {
        let out = "\
# branch.oid abc123
# branch.head main
1 .M N... 100644 100644 100644 h h file.go
1 .M N... 100644 100644 100644 h h other.rs
";
        let snap = parse(out);
        assert!(snap.dirty);
        let s = snap
            .dirty_summary
            .expect("dirty_summary present when dirty");
        assert_eq!(s.modified, 2);
        assert_eq!(s.added, 0);
        assert_eq!(s.deleted, 0);
        assert_eq!(s.untracked, 0);
        assert_eq!(s.renamed, 0);
    }

    #[test]
    fn dirty_untracked_only() {
        let out = "\
# branch.oid abc123
# branch.head main
? new1.txt
? new2.txt
? new3.txt
";
        let snap = parse(out);
        assert!(snap.dirty);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.untracked, 3);
        assert_eq!(s.modified, 0);
    }

    #[test]
    fn dirty_mixed() {
        let out = "\
# branch.oid abc123
# branch.head main
1 .M N... 100644 100644 100644 h h modified.go
1 A. N... 100644 100644 100644 h h added.go
1 .D N... 100644 100644 100644 h h deleted.go
2 R. N... 100644 100644 100644 h h R100 newpath.go\toldpath.go
? untracked.go
";
        let snap = parse(out);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.modified, 1);
        assert_eq!(s.added, 1);
        assert_eq!(s.deleted, 1);
        assert_eq!(s.renamed, 1);
        assert_eq!(s.untracked, 1);
    }

    #[test]
    fn no_upstream() {
        let out = "\
# branch.oid abc123
# branch.head feature-branch
";
        let snap = parse(out);
        assert_eq!(snap.branch, "feature-branch");
        assert!(snap.upstream.is_none());
        assert!(snap.ahead.is_none());
        assert!(snap.behind.is_none());
    }

    #[test]
    fn detached_head() {
        let out = "\
# branch.oid abc123def456
# branch.head (detached)
";
        let snap = parse(out);
        assert_eq!(snap.branch, "(detached)");
        assert_eq!(snap.last_commit_sha, "abc123def456");
    }

    #[test]
    fn empty_repo() {
        let out = "\
# branch.oid (initial)
# branch.head main
";
        let snap = parse(out);
        assert_eq!(snap.last_commit_sha, "(initial)");
        assert_eq!(snap.branch, "main");
        assert!(snap.upstream.is_none());
    }

    #[test]
    fn malformed_line_skipped() {
        let out = "\
# branch.oid abc123
# branch.head main
1 .M N... 100644 100644 100644 h h good.go
this is garbage that should be ignored
1 .M N... 100644 100644 100644 h h another.go
";
        let snap = parse(out);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.modified, 2);
    }

    #[test]
    fn path_with_spaces() {
        let out = "\
# branch.oid abc123
# branch.head main
1 .M N... 100644 100644 100644 h h file with spaces.go
? untracked file with spaces.txt
";
        let snap = parse(out);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.modified, 1);
        assert_eq!(s.untracked, 1);
    }

    #[test]
    fn mm_counts_modified_once() {
        // Staged-modified + unstaged-modified on the same file: one entry,
        // one increment to modified.
        let out = "\
# branch.oid abc123
# branch.head main
1 MM N... 100644 100644 100644 h h file.go
";
        let snap = parse(out);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.modified, 1);
    }

    #[test]
    fn ad_counts_independent() {
        // Staged add + unstaged delete on the same line. Both buckets tick.
        let out = "\
# branch.oid abc123
# branch.head main
1 AD N... 100644 100644 100644 h h file.go
";
        let snap = parse(out);
        let s = snap.dirty_summary.unwrap();
        assert_eq!(s.added, 1);
        assert_eq!(s.deleted, 1);
    }

    #[test]
    fn empty_stdout_returns_none() {
        assert!(parse_porcelain_v2("").is_none());
    }

    #[test]
    fn ahead_behind_parsed() {
        let out = "\
# branch.oid abc123
# branch.head main
# branch.upstream origin/main
# branch.ab +5 -2
";
        let snap = parse(out);
        assert_eq!(snap.ahead, Some(5));
        assert_eq!(snap.behind, Some(2));
    }

    #[test]
    fn captured_at_is_rfc3339() {
        let out = "\
# branch.oid abc123
# branch.head main
";
        let snap = parse(out);
        // RFC3339 starts with 4-digit year + dash; cheap shape check
        assert!(
            snap.captured_at.len() >= 20 && snap.captured_at.contains('T'),
            "captured_at not RFC3339: {}",
            snap.captured_at
        );
    }

    #[test]
    fn capture_in_non_repo_returns_none() {
        // /tmp is not a git repo — capture should return None, not panic.
        let snap = capture(std::path::Path::new("/tmp"));
        // We don't assert is_none() unconditionally because /tmp could be
        // inside a git worktree on some setups. The contract is "no panic,
        // no error" — that's what we test.
        let _ = snap;
    }

    // CULTRA-1080: inject_snapshot helper

    #[test]
    fn inject_when_context_snapshot_is_object_inserts_field() {
        let mut args = Map::new();
        args.insert(
            "context_snapshot".to_string(),
            json!({"next_session_start": "x"}),
        );
        inject_snapshot(&mut args, &fake_snapshot());
        let cs = args.get("context_snapshot").unwrap().as_object().unwrap();
        assert!(cs.contains_key("git_snapshot"));
        assert_eq!(cs["git_snapshot"]["branch"], "main");
        // Existing fields preserved.
        assert_eq!(cs["next_session_start"], "x");
    }

    #[test]
    fn inject_when_context_snapshot_missing_is_noop() {
        let mut args = Map::new();
        args.insert("project_id".to_string(), json!("proj-cultra"));
        inject_snapshot(&mut args, &fake_snapshot());
        // No context_snapshot synthesized.
        assert!(!args.contains_key("context_snapshot"));
    }

    #[test]
    fn inject_when_context_snapshot_is_not_object_is_noop() {
        let mut args = Map::new();
        args.insert(
            "context_snapshot".to_string(),
            json!("a string, not an object"),
        );
        inject_snapshot(&mut args, &fake_snapshot());
        // Field unchanged.
        assert_eq!(args["context_snapshot"], json!("a string, not an object"));
    }

    #[test]
    fn inject_overwrites_existing_git_snapshot() {
        // Auto-attach is authoritative: if an agent passed a stale git_snapshot,
        // the freshly captured one wins.
        let mut args = Map::new();
        args.insert(
            "context_snapshot".to_string(),
            json!({"git_snapshot": {"branch": "stale"}}),
        );
        inject_snapshot(&mut args, &fake_snapshot());
        assert_eq!(args["context_snapshot"]["git_snapshot"]["branch"], "main");
    }

    // CULTRA-1081: merge_current_git helper

    #[test]
    fn merge_current_git_into_object_inserts_field() {
        let mut dest = json!({"session": {"id": "abc"}});
        merge_current_git(&mut dest, &fake_snapshot());
        assert_eq!(dest["current_git"]["branch"], "main");
        // Sibling field preserved.
        assert_eq!(dest["session"]["id"], "abc");
    }

    #[test]
    fn merge_current_git_does_not_shadow_existing() {
        let mut dest = json!({"current_git": {"branch": "from-api"}});
        merge_current_git(&mut dest, &fake_snapshot());
        // Existing value wins.
        assert_eq!(dest["current_git"]["branch"], "from-api");
    }

    #[test]
    fn merge_current_git_into_non_object_is_noop() {
        let mut dest = json!("not an object");
        merge_current_git(&mut dest, &fake_snapshot());
        assert_eq!(dest, json!("not an object"));
    }
}
