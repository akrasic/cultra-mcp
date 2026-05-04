// AST parsing module - multi-language support
pub mod types;
pub mod parser;
mod languages;
mod analysis;
mod util;

pub use parser::Parser;
pub use analysis::complexity::{
    analyze_complexity, ComplexityAnalysis, ComplexitySummary, FunctionComplexity,
};
pub use analysis::concurrency::analyze_concurrency;
pub use analysis::concurrency_rust::analyze_concurrency_rust;
pub use analysis::css::{analyze_css, find_css_rules, find_unused_selectors, css_variable_graph};
pub use analysis::react::analyze_react_component;
pub use analysis::interfaces::find_interface_implementations;
pub use analysis::security::analyze_security;
pub use analysis::tailwind::resolve_tailwind_classes;
