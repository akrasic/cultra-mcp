// Language-specific AST extractors
pub mod go;
pub mod typescript;
pub mod python;
pub mod rust_lang;

// Re-export main extraction functions
pub use go::extract_go_symbols;
pub use typescript::extract_typescript_symbols;
pub use python::extract_python_symbols;
pub use rust_lang::extract_rust_symbols;
