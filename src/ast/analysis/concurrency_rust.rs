// CULTRA-910: Rust concurrency analyzer.
//
// The existing src/ast/analysis/concurrency.rs is Go-only (extracts goroutines,
// chan types, select{} statements, etc.). This module provides the equivalent
// for Rust using tree-sitter-rust queries:
//
//   - Spawn points (tokio::spawn, std::thread::spawn, rayon::spawn, ...)
//   - Sync primitives (Mutex<T>, RwLock<T>, AtomicX, parking_lot, tokio::sync::*)
//   - Channels (mpsc, broadcast, oneshot, watch, crossbeam, flume)
//   - select! macro invocations
//   - async fn definitions and .await counts
//
// Scope decisions (per the audit task):
//   - V1 covers tokio + std::thread + parking_lot + crossbeam + rayon. async-std
//     is uncommon today and can be added incrementally.
//   - Pattern detection only — no race-condition / deadlock heuristics. Rust's
//     type system catches most of those at compile time.
//   - Send/Sync bound analysis is out of scope for V1.

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use tree_sitter::Node;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustConcurrencyAnalysis {
    pub language: String, // always "rust"
    pub spawns: Vec<RustSpawn>,
    pub synchronization: Vec<RustSyncPrimitive>,
    pub channels: Vec<RustChannel>,
    pub selects: Vec<RustSelect>,
    pub async_functions: Vec<RustAsyncFn>,
    pub await_points: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustSpawn {
    pub kind: String, // "tokio::spawn", "std::thread::spawn", ...
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustSyncPrimitive {
    pub kind: String, // "Mutex", "RwLock", "Semaphore", "AtomicU64", "parking_lot::Mutex", ...
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustChannel {
    pub kind: String, // "mpsc::channel", "tokio::sync::broadcast::channel", ...
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustSelect {
    pub kind: String, // "tokio::select" | "futures::select"
    pub location: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RustAsyncFn {
    pub name: String,
    pub location: String,
}

/// Top-level entry. Reads the file, parses it with tree-sitter-rust, walks
/// the tree extracting concurrency-related symbols.
pub fn analyze_concurrency_rust(file_path: &str) -> Result<RustConcurrencyAnalysis> {
    let content = fs::read_to_string(file_path)?;
    let bytes = content.as_bytes();

    let mut parser = tree_sitter::Parser::new();
    let lang: tree_sitter::Language = tree_sitter_rust::LANGUAGE.into();
    parser.set_language(&lang)?;

    let tree = parser
        .parse(&content, None)
        .ok_or_else(|| anyhow::anyhow!("Failed to parse Rust file"))?;

    let root = tree.root_node();
    let loc = |n: Node| -> String {
        format!("{}:{}", file_path, n.start_position().row + 1)
    };

    let mut spawns = Vec::new();
    let mut synchronization = Vec::new();
    let mut channels = Vec::new();
    let mut selects = Vec::new();
    let mut async_functions = Vec::new();
    let mut await_points = 0usize;

    // Walk every node once. Tree-sitter Query is more elegant but the patterns
    // we need are simple enough that a single walk keeps the diff small and
    // avoids constructing per-feature queries.
    walk(root, bytes, &mut |node| {
        match node.kind() {
            "call_expression" => {
                let func_text = node
                    .child_by_field_name("function")
                    .map(|f| node_text(f, bytes));
                if let Some(text) = func_text {
                    // Spawns: full match on the path text.
                    if let Some(kind) = classify_spawn(&text) {
                        spawns.push(RustSpawn { kind, location: loc(node) });
                    }
                    // Channels: same.
                    if let Some(kind) = classify_channel(&text) {
                        channels.push(RustChannel { kind, location: loc(node) });
                    }
                }
            }
            "generic_type" => {
                // Mutex<T>, RwLock<T>, Semaphore, etc. — captures type name.
                if let Some(name_node) = node.child_by_field_name("type") {
                    let name = node_text(name_node, bytes);
                    if let Some(kind) = classify_sync_type(&name) {
                        synchronization.push(RustSyncPrimitive { kind, location: loc(node) });
                    }
                }
            }
            "scoped_type_identifier" | "type_identifier" => {
                // Catches `parking_lot::Mutex` and bare `AtomicU64`.
                let text = node_text(node, bytes);
                if let Some(kind) = classify_sync_type(&text) {
                    synchronization.push(RustSyncPrimitive { kind, location: loc(node) });
                }
            }
            "macro_invocation" => {
                if let Some(macro_node) = node.child_by_field_name("macro") {
                    let name = node_text(macro_node, bytes);
                    if name == "select" || name.ends_with("::select") {
                        let kind = if name.starts_with("tokio::") {
                            "tokio::select".to_string()
                        } else if name.starts_with("futures::") {
                            "futures::select".to_string()
                        } else {
                            "select".to_string()
                        };
                        selects.push(RustSelect { kind, location: loc(node) });
                    }
                }
            }
            "function_item" => {
                if is_async_function(node, bytes) {
                    let name = node
                        .child_by_field_name("name")
                        .map(|n| node_text(n, bytes))
                        .unwrap_or_else(|| "<anonymous>".to_string());
                    async_functions.push(RustAsyncFn {
                        name,
                        location: loc(node),
                    });
                }
            }
            "await_expression" => {
                await_points += 1;
            }
            _ => {}
        }
    });

    // Deduplicate sync primitives by (location, normalized bare name).
    // CULTRA-944: walking generic_type AND scoped_type_identifier yielded
    // duplicates for patterns like `Arc<std::sync::Mutex<T>>` because the
    // `kind` strings differ ("Mutex" vs "std::sync::Mutex") even though the
    // underlying node maps to the same source position. Comparing on the
    // bare name (last `::` segment) normalizes across both forms; we keep
    // the more qualified entry (prefer the one whose kind contains "::").
    let bare = |kind: &str| -> String {
        kind.rsplit("::").next().unwrap_or(kind).to_string()
    };
    synchronization.sort_by(|a, b| {
        let (ba, bb) = (bare(&a.kind), bare(&b.kind));
        (&a.location, &ba, a.kind.contains("::"))
            .cmp(&(&b.location, &bb, b.kind.contains("::")))
    });
    // After sort, the qualified form sorts AFTER the bare form at the same
    // (location, bare_name). Dedup keeps the FIRST element; reverse so the
    // qualified form (more informative) survives instead of the bare one.
    synchronization.reverse();
    synchronization.dedup_by(|a, b| a.location == b.location && bare(&a.kind) == bare(&b.kind));
    synchronization.reverse();

    Ok(RustConcurrencyAnalysis {
        language: "rust".to_string(),
        spawns,
        synchronization,
        channels,
        selects,
        async_functions,
        await_points,
    })
}

fn walk(node: Node, bytes: &[u8], visit: &mut dyn FnMut(Node)) {
    visit(node);
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        walk(child, bytes, visit);
    }
}

fn node_text(n: Node, bytes: &[u8]) -> String {
    String::from_utf8_lossy(&bytes[n.byte_range()]).into_owned()
}

/// True if the function_item has an `async` modifier. tree-sitter-rust
/// represents this as a `function_modifiers` child whose first token is `async`.
fn is_async_function(node: Node, bytes: &[u8]) -> bool {
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "function_modifiers" {
            return node_text(child, bytes).contains("async");
        }
    }
    false
}

/// Classify a call_expression's function path text as a known spawn point.
fn classify_spawn(text: &str) -> Option<String> {
    // Strict match on the recognized prefixes. We avoid greedy matching so a
    // user-defined `my_module::tokio_spawn` doesn't get flagged.
    const SPAWNS: &[&str] = &[
        "tokio::spawn",
        "tokio::task::spawn",
        "tokio::task::spawn_blocking",
        "tokio::task::spawn_local",
        "task::spawn",
        "task::spawn_blocking",
        "std::thread::spawn",
        "thread::spawn",
        "rayon::spawn",
    ];
    // CULTRA-958: same turbofish-strip as classify_channel.
    let stripped = strip_turbofish(text);
    for s in SPAWNS {
        if stripped == *s {
            return Some((*s).to_string());
        }
    }
    None
}

/// Classify a call_expression's function path text as a known channel constructor.
fn classify_channel(text: &str) -> Option<String> {
    const CHANNELS: &[&str] = &[
        "mpsc::channel",
        "mpsc::sync_channel",
        "mpsc::unbounded_channel",
        "std::sync::mpsc::channel",
        "std::sync::mpsc::sync_channel",
        "tokio::sync::mpsc::channel",
        "tokio::sync::mpsc::unbounded_channel",
        "tokio::sync::broadcast::channel",
        "tokio::sync::oneshot::channel",
        "tokio::sync::watch::channel",
        "broadcast::channel",
        "oneshot::channel",
        "watch::channel",
        "crossbeam::channel::bounded",
        "crossbeam::channel::unbounded",
        "crossbeam_channel::bounded",
        "crossbeam_channel::unbounded",
        "flume::bounded",
        "flume::unbounded",
    ];
    // CULTRA-958: strip turbofish (`::<...>`) before matching so calls like
    // `mpsc::channel::<u32>(16)` are recognized. Tree-sitter-rust's
    // `function` field on a call_expression preserves the turbofish in the
    // text, so without this strip, exact-match misses the common
    // typed-constructor case.
    let stripped = strip_turbofish(text);
    for c in CHANNELS {
        if stripped == *c {
            return Some((*c).to_string());
        }
    }
    None
}

/// CULTRA-958: strip Rust turbofish syntax (`::<...>`) from a call path
/// so classifiers can match the bare function name. Returns the prefix
/// before `::<` if present, else the original text. Conservative — only
/// strips the FIRST turbofish, which covers `path::fn::<T>` and
/// `path::fn::<T1, T2>(args)` but bails on more exotic constructs.
fn strip_turbofish(text: &str) -> &str {
    match text.find("::<") {
        Some(idx) => &text[..idx],
        None => text,
    }
}

/// Classify a type identifier (bare or scoped) as a sync primitive. Returns
/// the canonical name we want to report.
fn classify_sync_type(text: &str) -> Option<String> {
    // Strip leading qualifier so `parking_lot::Mutex` and `Mutex` both match,
    // but report the canonical name with the qualifier when present.
    let bare = text.rsplit("::").next().unwrap_or(text);
    let recognized = matches!(
        bare,
        "Mutex"
            | "RwLock"
            | "Semaphore"
            | "Barrier"
            | "Notify"
            | "OnceCell"
            | "OnceLock"
    ) || bare.starts_with("Atomic");
    if recognized {
        Some(text.to_string())
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::tempdir;

    fn write_rs(name: &str, contents: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join(name);
        let mut f = fs::File::create(&path).unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn test_extracts_tokio_spawn() {
        let (_d, p) = write_rs(
            "spawn.rs",
            r#"
async fn main() {
    tokio::spawn(async move {
        println!("hi");
    });
}
"#,
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert_eq!(analysis.language, "rust");
        assert!(
            analysis.spawns.iter().any(|s| s.kind == "tokio::spawn"),
            "expected tokio::spawn, got {:?}",
            analysis.spawns
        );
    }

    #[test]
    fn test_extracts_std_thread_spawn() {
        let (_d, p) = write_rs(
            "thr.rs",
            "fn main() {\n    std::thread::spawn(|| {\n        println!(\"hi\");\n    });\n}\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert!(analysis.spawns.iter().any(|s| s.kind == "std::thread::spawn"));
    }

    #[test]
    fn test_extracts_arc_mutex_t() {
        let (_d, p) = write_rs(
            "mtx.rs",
            "use std::sync::{Arc, Mutex};\nfn main() {\n    let _x: Arc<Mutex<i32>> = Arc::new(Mutex::new(0));\n}\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert!(
            analysis.synchronization.iter().any(|s| s.kind == "Mutex"),
            "expected Mutex sync primitive, got {:?}",
            analysis.synchronization
        );
    }

    #[test]
    fn test_extracts_mpsc_channel() {
        let (_d, p) = write_rs(
            "chan.rs",
            "fn main() {\n    let (_tx, _rx) = std::sync::mpsc::channel();\n}\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert!(
            analysis.channels.iter().any(|c| c.kind == "std::sync::mpsc::channel"),
            "expected std::sync::mpsc::channel, got {:?}",
            analysis.channels
        );
    }

    #[test]
    fn test_extracts_mpsc_channel_with_turbofish() {
        // CULTRA-958: typed-constructor pattern `mpsc::channel::<T>(N)` is
        // common in real Rust code (e.g. when type inference can't pick the
        // element type). The pre-fix exact-match classifier failed on it
        // because the function path text included `::<u32>`. Fixed by
        // strip_turbofish.
        let (_d, p) = write_rs(
            "turbo.rs",
            "fn main() {\n    let (_tx, _rx) = std::sync::mpsc::channel::<u32>();\n}\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert!(
            analysis.channels.iter().any(|c| c.kind == "std::sync::mpsc::channel"),
            "turbofish constructor must be detected, got: {:?}",
            analysis.channels
        );
    }

    #[test]
    fn test_strip_turbofish_unit() {
        assert_eq!(strip_turbofish("mpsc::channel"), "mpsc::channel");
        assert_eq!(strip_turbofish("mpsc::channel::<u32>"), "mpsc::channel");
        assert_eq!(strip_turbofish("path::fn::<T1, T2>"), "path::fn");
        assert_eq!(strip_turbofish(""), "");
        // No turbofish, no change
        assert_eq!(strip_turbofish("std::thread::spawn"), "std::thread::spawn");
    }

    #[test]
    fn test_extracts_async_fn_and_await() {
        let (_d, p) = write_rs(
            "asn.rs",
            "async fn fetch() -> i32 {\n    let x = other().await;\n    x\n}\nasync fn other() -> i32 { 1 }\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        let names: Vec<_> = analysis.async_functions.iter().map(|f| f.name.clone()).collect();
        assert!(names.contains(&"fetch".to_string()), "got {:?}", names);
        assert!(names.contains(&"other".to_string()), "got {:?}", names);
        assert!(analysis.await_points >= 1, "expected at least one await");
    }

    #[test]
    fn test_extracts_select_macro() {
        let (_d, p) = write_rs(
            "sel.rs",
            r#"
async fn run() {
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(1)) => {}
    }
}
"#,
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert!(
            !analysis.selects.is_empty(),
            "expected at least one select macro, got {:?}",
            analysis.selects
        );
    }

    #[test]
    fn test_empty_file_returns_empty_analysis() {
        let (_d, p) = write_rs("empty.rs", "");
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();
        assert_eq!(analysis.language, "rust");
        assert!(analysis.spawns.is_empty());
        assert!(analysis.channels.is_empty());
        assert!(analysis.synchronization.is_empty());
        assert!(analysis.selects.is_empty());
        assert!(analysis.async_functions.is_empty());
        assert_eq!(analysis.await_points, 0);
    }

    #[test]
    fn test_dedup_qualified_vs_bare_mutex() {
        // CULTRA-944: Arc<std::sync::Mutex<T>> previously reported the Mutex
        // twice — once as "Mutex" (bare type_identifier) and once as
        // "std::sync::Mutex" (scoped_type_identifier) on the same line.
        // Regression guard: exactly one entry should remain, and it should
        // be the more qualified form.
        let (_d, p) = write_rs(
            "dup.rs",
            "use std::sync::{Arc, Mutex};\nfn main() {\n    let _x: Arc<std::sync::Mutex<i32>> = Arc::new(Mutex::new(0));\n}\n",
        );
        let analysis = analyze_concurrency_rust(p.to_str().unwrap()).unwrap();

        // Count Mutex-class primitives (normalized by bare name).
        let mutexes: Vec<_> = analysis
            .synchronization
            .iter()
            .filter(|s| s.kind.rsplit("::").next() == Some("Mutex"))
            .collect();
        // Exactly one per source line — no dup. There are two Mutex sites
        // in the test: the `Arc<std::sync::Mutex<i32>>` type (line 3) AND
        // the `Mutex::new(0)` call... wait, Mutex::new is a call, not a
        // type. Only line 3 should register.
        assert_eq!(
            mutexes.len(),
            1,
            "expected exactly 1 Mutex after dedup, got {:?}",
            mutexes
        );
        // Prefer the qualified form in the output.
        assert_eq!(mutexes[0].kind, "std::sync::Mutex");
    }

    #[test]
    fn test_dogfood_lsp_manager_arc_mutex() {
        // The mcp-server-rust LSP manager is a known Arc<Mutex<...>> user.
        // We resolve relative to CARGO_MANIFEST_DIR so the test works
        // regardless of where cargo test is invoked from.
        let manifest = env!("CARGO_MANIFEST_DIR");
        let path = std::path::Path::new(manifest)
            .join("src")
            .join("lsp")
            .join("manager.rs");
        if !path.exists() {
            // Skip if the source layout has changed; this is a dog-food
            // test, not a contract.
            return;
        }
        let analysis = analyze_concurrency_rust(path.to_str().unwrap()).unwrap();
        assert!(
            analysis.synchronization.iter().any(|s| s.kind.contains("Mutex")),
            "expected at least one Mutex in lsp/manager.rs, got {:?}",
            analysis.synchronization
        );
    }

}
