// Language-specific AST extractors
pub mod go;
pub mod python;
pub mod rust_lang;
pub mod terraform;
pub mod typescript;

// Re-export main extraction functions
pub use go::extract_go_symbols;
pub use python::extract_python_symbols;
pub use rust_lang::extract_rust_symbols;
pub use terraform::extract_terraform_symbols;
pub use typescript::extract_typescript_symbols;
