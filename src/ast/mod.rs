// AST parsing module - multi-language support
mod analysis;
mod languages;
pub mod parser;
pub mod types;
mod util;

pub use analysis::complexity::{
    analyze_complexity, ComplexityAnalysis, ComplexitySummary, FunctionComplexity,
};
pub use analysis::concurrency::analyze_concurrency;
pub use analysis::concurrency_rust::analyze_concurrency_rust;
pub use analysis::css::{analyze_css, css_variable_graph, find_css_rules, find_unused_selectors};
pub use analysis::interfaces::find_interface_implementations;
pub use analysis::react::analyze_react_component;
pub use analysis::security::analyze_security;
pub use analysis::tailwind::resolve_tailwind_classes;
pub use parser::Parser;
