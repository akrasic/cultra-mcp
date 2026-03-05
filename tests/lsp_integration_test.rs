// Integration tests for LSP functionality
//
// These tests require LSP servers to be installed:
// - gopls (Go): go install golang.org/x/tools/gopls@latest
// - rust-analyzer: rustup component add rust-analyzer
// - typescript-language-server: npm install -g typescript-language-server
//
// Run with: cargo test --test lsp_integration_test -- --ignored

use cultra_mcp::lsp::LSPManager;
use serde_json::{Map, Value};
use std::path::PathBuf;

// Helper to check if an LSP server is available
fn is_lsp_server_available(language: &str) -> bool {
    let binary = match language {
        "go" => "gopls",
        "rust" => "rust-analyzer",
        "typescript" => "typescript-language-server",
        _ => return false,
    };

    which::which(binary).is_ok()
}

// Helper to get test workspace root
fn get_test_workspace() -> PathBuf {
    // Use CARGO_MANIFEST_DIR for reliable crate root detection
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    manifest_dir.parent().unwrap_or(&manifest_dir).to_path_buf()
}

// Helper to get a test file path
fn get_test_file(language: &str) -> Option<PathBuf> {
    let workspace = get_test_workspace();

    match language {
        "go" => {
            // Find a Go file in v2/ directory
            let go_file = workspace.join("v2/internal/api/server.go");
            if go_file.exists() {
                Some(go_file)
            } else {
                eprintln!("Go file not found at: {:?}", go_file);
                None
            }
        }
        "rust" => {
            // Use our own source file
            let rs_file = workspace.join("mcp-server-rust/src/lib.rs");
            if rs_file.exists() {
                Some(rs_file)
            } else {
                eprintln!("Rust file not found at: {:?}", rs_file);
                None
            }
        }
        _ => None,
    }
}

// Helper to create an LSP manager for tests
fn test_lsp_manager() -> LSPManager {
    LSPManager::new(get_test_workspace())
}

#[test]
#[ignore]
fn test_lsp_document_symbols_go() {
    if !is_lsp_server_available("go") {
        eprintln!("Skipping test: gopls not found in PATH");
        return;
    }

    let test_file = match get_test_file("go") {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: No Go test file found");
            return;
        }
    };

    println!("Testing lsp_document_symbols with file: {:?}", test_file);

    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String(test_file.to_string_lossy().to_string()),
    );
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().to_string_lossy().to_string()),
    );

    let result = cultra_mcp::lsp::tools::lsp_document_symbols(args, &lsp);

    match result {
        Ok(response) => {
            println!("Response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();

            assert!(obj.contains_key("symbols"));
            assert!(obj.contains_key("count"));
            assert!(obj.contains_key("format"));

            let count = obj.get("count").unwrap().as_u64().unwrap();
            assert!(count > 0, "Should find at least some symbols");

            let symbols = obj.get("symbols").unwrap().as_array().unwrap();
            assert_eq!(symbols.len() as u64, count);

            if let Some(first_symbol) = symbols.first() {
                let symbol = first_symbol.as_object().unwrap();
                assert!(symbol.contains_key("name"));
                assert!(symbol.contains_key("kind"));
                assert!(symbol.contains_key("location"));
            }
        }
        Err(e) => {
            panic!("lsp_document_symbols failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_document_symbols_rust() {
    if !is_lsp_server_available("rust") {
        eprintln!("Skipping test: rust-analyzer not found in PATH");
        return;
    }

    let test_file = match get_test_file("rust") {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: No Rust test file found");
            return;
        }
    };

    println!("Testing lsp_document_symbols with Rust file: {:?}", test_file);

    let lsp = LSPManager::new(get_test_workspace().join("mcp-server-rust"));
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String(test_file.to_string_lossy().to_string()),
    );
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().join("mcp-server-rust").to_string_lossy().to_string()),
    );

    let result = cultra_mcp::lsp::tools::lsp_document_symbols(args, &lsp);

    match result {
        Ok(response) => {
            println!("Response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();
            let count = obj.get("count").unwrap().as_u64().unwrap();
            assert!(count > 0, "Should find symbols in lib.rs");
        }
        Err(e) => {
            panic!("lsp_document_symbols failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_hover_go() {
    if !is_lsp_server_available("go") {
        eprintln!("Skipping test: gopls not found in PATH");
        return;
    }

    let test_file = match get_test_file("go") {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: No Go test file found");
            return;
        }
    };

    println!("Testing lsp_hover with file: {:?}", test_file);

    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String(test_file.to_string_lossy().to_string()),
    );
    args.insert("line".to_string(), Value::Number(34.into()));
    args.insert("character".to_string(), Value::Number(6.into()));
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().to_string_lossy().to_string()),
    );

    args.insert("action".to_string(), Value::String("hover".to_string()));
    let result = cultra_mcp::lsp::tools::lsp_query(args, &lsp);

    match result {
        Ok(response) => {
            println!("Hover response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();

            if obj.get("found").unwrap().as_bool().unwrap() {
                assert!(obj.contains_key("contents"));
                let contents = obj.get("contents").unwrap().as_str().unwrap();
                assert!(!contents.is_empty(), "Contents should not be empty");
            }
        }
        Err(e) => {
            panic!("lsp_query(hover) failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_goto_definition_go() {
    if !is_lsp_server_available("go") {
        eprintln!("Skipping test: gopls not found in PATH");
        return;
    }

    let test_file = match get_test_file("go") {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: No Go test file found");
            return;
        }
    };

    println!("Testing lsp_goto_definition with file: {:?}", test_file);

    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String(test_file.to_string_lossy().to_string()),
    );
    args.insert("line".to_string(), Value::Number(50.into()));
    args.insert("character".to_string(), Value::Number(10.into()));
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().to_string_lossy().to_string()),
    );

    args.insert("action".to_string(), Value::String("definition".to_string()));
    let result = cultra_mcp::lsp::tools::lsp_query(args, &lsp);

    match result {
        Ok(response) => {
            println!("Goto definition response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();
            assert!(obj.contains_key("found"));

            if obj.get("found").unwrap().as_bool().unwrap() {
                assert!(obj.contains_key("location"));
            }
        }
        Err(e) => {
            panic!("lsp_query(definition) failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_find_references_go() {
    if !is_lsp_server_available("go") {
        eprintln!("Skipping test: gopls not found in PATH");
        return;
    }

    let test_file = match get_test_file("go") {
        Some(f) => f,
        None => {
            eprintln!("Skipping test: No Go test file found");
            return;
        }
    };

    println!("Testing lsp_find_references with file: {:?}", test_file);

    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String(test_file.to_string_lossy().to_string()),
    );
    args.insert("line".to_string(), Value::Number(34.into()));
    args.insert("character".to_string(), Value::Number(6.into()));
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().to_string_lossy().to_string()),
    );

    args.insert("action".to_string(), Value::String("references".to_string()));
    let result = cultra_mcp::lsp::tools::lsp_query(args, &lsp);

    match result {
        Ok(response) => {
            println!("Find references response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();
            assert!(obj.contains_key("references"));
            assert!(obj.contains_key("count"));

            let count = obj.get("count").unwrap().as_u64().unwrap();
            let references = obj.get("references").unwrap().as_array().unwrap();
            assert_eq!(references.len() as u64, count);
        }
        Err(e) => {
            panic!("lsp_query(references) failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_workspace_symbols_go() {
    if !is_lsp_server_available("go") {
        eprintln!("Skipping test: gopls not found in PATH");
        return;
    }

    println!("Testing lsp_workspace_symbols");

    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert("query".to_string(), Value::String("Server".to_string()));
    args.insert("language".to_string(), Value::String("go".to_string()));
    args.insert(
        "workspace_root".to_string(),
        Value::String(get_test_workspace().to_string_lossy().to_string()),
    );

    let result = cultra_mcp::lsp::tools::lsp_workspace_symbols(args, &lsp);

    match result {
        Ok(response) => {
            println!("Workspace symbols response: {}", serde_json::to_string_pretty(&response).unwrap());

            let obj = response.as_object().unwrap();
            assert!(obj.contains_key("symbols"));
            assert!(obj.contains_key("count"));

            let count = obj.get("count").unwrap().as_u64().unwrap();
            let symbols = obj.get("symbols").unwrap().as_array().unwrap();
            assert_eq!(symbols.len() as u64, count);

            if count > 0 {
                let first = symbols.first().unwrap().as_object().unwrap();
                assert!(first.contains_key("name"));
            }
        }
        Err(e) => {
            panic!("lsp_workspace_symbols failed: {}", e);
        }
    }
}

#[test]
#[ignore]
fn test_lsp_error_handling_invalid_file() {
    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String("/nonexistent/file.go".to_string()),
    );

    let result = cultra_mcp::lsp::tools::lsp_document_symbols(args, &lsp);
    assert!(result.is_err(), "Should error on non-existent file");
}

#[test]
#[ignore]
fn test_lsp_error_handling_unsupported_language() {
    let lsp = test_lsp_manager();
    let mut args = Map::new();
    args.insert(
        "file_path".to_string(),
        Value::String("/path/to/file.xyz".to_string()),
    );

    let result = cultra_mcp::lsp::tools::lsp_document_symbols(args, &lsp);
    assert!(result.is_err(), "Should error on unsupported language");
}
