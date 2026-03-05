use serde::{Deserialize, Serialize};
use std::fmt;

// ========== Enum Helper Trait ==========

/// Trait to provide all valid values for an enum
pub trait EnumValues {
    fn valid_values() -> Vec<String>;
}

// ========== Task Enums ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskType {
    Feature,
    Bug,
    Chore,
    Research,
}

impl fmt::Display for TaskType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskType::Feature => write!(f, "feature"),
            TaskType::Bug => write!(f, "bug"),
            TaskType::Chore => write!(f, "chore"),
            TaskType::Research => write!(f, "research"),
        }
    }
}

impl EnumValues for TaskType {
    fn valid_values() -> Vec<String> {
        vec![
            "feature".to_string(),
            "bug".to_string(),
            "chore".to_string(),
            "research".to_string(),
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Todo,
    InProgress,
    Blocked,
    Done,
    Cancelled,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Todo => write!(f, "todo"),
            TaskStatus::InProgress => write!(f, "in_progress"),
            TaskStatus::Blocked => write!(f, "blocked"),
            TaskStatus::Done => write!(f, "done"),
            TaskStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

impl EnumValues for TaskStatus {
    fn valid_values() -> Vec<String> {
        vec![
            "todo".to_string(),
            "in_progress".to_string(),
            "blocked".to_string(),
            "done".to_string(),
            "cancelled".to_string(),
        ]
    }
}

// ========== Priority Enum (shared by Task & Plan) ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum Priority {
    P0, // Highest priority
    P1,
    P2,
    P3, // Lowest priority
}

impl fmt::Display for Priority {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Priority::P0 => write!(f, "P0"),
            Priority::P1 => write!(f, "P1"),
            Priority::P2 => write!(f, "P2"),
            Priority::P3 => write!(f, "P3"),
        }
    }
}

impl EnumValues for Priority {
    fn valid_values() -> Vec<String> {
        vec![
            "P0".to_string(),
            "P1".to_string(),
            "P2".to_string(),
            "P3".to_string(),
        ]
    }
}

// ========== Plan Enums ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStatus {
    Draft,
    InProgress,
    Completed,
}

impl fmt::Display for PlanStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PlanStatus::Draft => write!(f, "draft"),
            PlanStatus::InProgress => write!(f, "in_progress"),
            PlanStatus::Completed => write!(f, "completed"),
        }
    }
}

impl EnumValues for PlanStatus {
    fn valid_values() -> Vec<String> {
        vec![
            "draft".to_string(),
            "in_progress".to_string(),
            "completed".to_string(),
        ]
    }
}

// ========== Decision Enums ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionStatus {
    Proposed,
    Accepted,
    Deprecated,
    Superseded,
}

impl fmt::Display for DecisionStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DecisionStatus::Proposed => write!(f, "proposed"),
            DecisionStatus::Accepted => write!(f, "accepted"),
            DecisionStatus::Deprecated => write!(f, "deprecated"),
            DecisionStatus::Superseded => write!(f, "superseded"),
        }
    }
}

impl EnumValues for DecisionStatus {
    fn valid_values() -> Vec<String> {
        vec![
            "proposed".to_string(),
            "accepted".to_string(),
            "deprecated".to_string(),
            "superseded".to_string(),
        ]
    }
}

// ========== Document Enums ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DocType {
    // V2 types (existing)
    Guide,
    TestReport,
    Decision,
    Architecture,
    // Engine V3 types (NEW)
    PlanDetails,
    Implementation,
    Retrospective,
    General,
    Offtopic,
    #[serde(other)]
    Other,
}

impl fmt::Display for DocType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            // V2 types
            DocType::Guide => write!(f, "guide"),
            DocType::TestReport => write!(f, "test_report"),
            DocType::Decision => write!(f, "decision"),
            DocType::Architecture => write!(f, "architecture"),
            // Engine V3 types
            DocType::PlanDetails => write!(f, "plan_details"),
            DocType::Implementation => write!(f, "implementation"),
            DocType::Retrospective => write!(f, "retrospective"),
            DocType::General => write!(f, "general"),
            DocType::Offtopic => write!(f, "offtopic"),
            DocType::Other => write!(f, "other"),
        }
    }
}

impl EnumValues for DocType {
    fn valid_values() -> Vec<String> {
        vec![
            "guide".to_string(),
            "test_report".to_string(),
            "decision".to_string(),
            "architecture".to_string(),
            "plan_details".to_string(),
            "implementation".to_string(),
            "retrospective".to_string(),
            "general".to_string(),
            "offtopic".to_string(),
        ]
    }
}

// ========== Background Job Enums ==========

#[allow(dead_code)] // Not yet exposed via MCP tools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    Pending,
    Processing,
    Completed,
    Failed,
    Retrying,
    Cancelled,
}

impl fmt::Display for JobStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobStatus::Pending => write!(f, "pending"),
            JobStatus::Processing => write!(f, "processing"),
            JobStatus::Completed => write!(f, "completed"),
            JobStatus::Failed => write!(f, "failed"),
            JobStatus::Retrying => write!(f, "retrying"),
            JobStatus::Cancelled => write!(f, "cancelled"),
        }
    }
}

#[allow(dead_code)] // Not yet exposed via MCP tools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobType {
    EmbedDocument,
    EmbedTask,
    EmbedSession,
    EmbedSymbol,
    BatchIndex,
    #[serde(other)]
    Other,
}

impl fmt::Display for JobType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            JobType::EmbedDocument => write!(f, "embed_document"),
            JobType::EmbedTask => write!(f, "embed_task"),
            JobType::EmbedSession => write!(f, "embed_session"),
            JobType::EmbedSymbol => write!(f, "embed_symbol"),
            JobType::BatchIndex => write!(f, "batch_index"),
            JobType::Other => write!(f, "other"),
        }
    }
}

// ========== Log Level Enum ==========

#[allow(dead_code)] // Not yet exposed via MCP tools
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    Debug,
    Info,
    Warn,
    Error,
}

impl fmt::Display for LogLevel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LogLevel::Debug => write!(f, "debug"),
            LogLevel::Info => write!(f, "info"),
            LogLevel::Warn => write!(f, "warn"),
            LogLevel::Error => write!(f, "error"),
        }
    }
}

// ========== Graph Enums ==========

#[allow(dead_code)] // Not yet exposed via MCP tools (future: graph operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EdgeClass {
    Structural,  // Strong, explicit relationships (dependencies, hierarchy)
    Evidence,    // Medium strength, observed relationships (calls, uses)
    Heuristic,   // Weak, inferred relationships (similarity, co-occurrence)
}

impl fmt::Display for EdgeClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EdgeClass::Structural => write!(f, "STRUCTURAL"),
            EdgeClass::Evidence => write!(f, "EVIDENCE"),
            EdgeClass::Heuristic => write!(f, "HEURISTIC"),
        }
    }
}

#[allow(dead_code)] // Not yet exposed via MCP tools (future: graph operations)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntityType {
    Session,
    Task,
    Plan,
    Document,
    Decision,
    Project,
    Symbol,
    #[serde(other)]
    Other,
}

impl fmt::Display for EntityType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            EntityType::Session => write!(f, "session"),
            EntityType::Task => write!(f, "task"),
            EntityType::Plan => write!(f, "plan"),
            EntityType::Document => write!(f, "document"),
            EntityType::Decision => write!(f, "decision"),
            EntityType::Project => write!(f, "project"),
            EntityType::Symbol => write!(f, "symbol"),
            EntityType::Other => write!(f, "other"),
        }
    }
}

// ========== Session Strategy Enum ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SessionStrategy {
    Latest,    // Most recently active session
    Relevant,  // Highest retrievability score (recency + access patterns)
    Merge,     // Combine multiple high-scoring sessions (future)
}

impl Default for SessionStrategy {
    fn default() -> Self {
        SessionStrategy::Latest
    }
}

impl fmt::Display for SessionStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SessionStrategy::Latest => write!(f, "latest"),
            SessionStrategy::Relevant => write!(f, "relevant"),
            SessionStrategy::Merge => write!(f, "merge"),
        }
    }
}

impl EnumValues for SessionStrategy {
    fn valid_values() -> Vec<String> {
        vec![
            "latest".to_string(),
            "relevant".to_string(),
            "merge".to_string(),
        ]
    }
}

// ========== AST/Code Analysis Enums ==========

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Language {
    Go,
    Typescript,
    Javascript,
    Python,
    Rust,
    Php,
    #[serde(other)]
    Other,
}

impl fmt::Display for Language {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Language::Go => write!(f, "go"),
            Language::Typescript => write!(f, "typescript"),
            Language::Javascript => write!(f, "javascript"),
            Language::Python => write!(f, "python"),
            Language::Rust => write!(f, "rust"),
            Language::Php => write!(f, "php"),
            Language::Other => write!(f, "other"),
        }
    }
}

impl Language {
    pub fn from_extension(ext: &str) -> Self {
        match ext {
            "go" => Language::Go,
            "ts" | "tsx" => Language::Typescript,
            "js" | "jsx" => Language::Javascript,
            "py" => Language::Python,
            "rs" => Language::Rust,
            "php" => Language::Php,
            _ => Language::Other,
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "go" | "golang" => Language::Go,
            "typescript" | "ts" => Language::Typescript,
            "javascript" | "js" => Language::Javascript,
            "python" | "py" => Language::Python,
            "rust" | "rs" => Language::Rust,
            "php" => Language::Php,
            _ => Language::Other,
        }
    }
}

impl std::str::FromStr for Language {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "go" | "golang" => Language::Go,
            "typescript" | "ts" => Language::Typescript,
            "javascript" | "js" => Language::Javascript,
            "python" | "py" => Language::Python,
            "rust" | "rs" => Language::Rust,
            "php" => Language::Php,
            _ => Language::Other,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SymbolType {
    Function,
    Method,
    Type,
    Interface,
    Class,
    Struct,
    Enum,
    Constant,
    Variable,
    #[serde(other)]
    Other,
}

impl fmt::Display for SymbolType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SymbolType::Function => write!(f, "function"),
            SymbolType::Method => write!(f, "method"),
            SymbolType::Type => write!(f, "type"),
            SymbolType::Interface => write!(f, "interface"),
            SymbolType::Class => write!(f, "class"),
            SymbolType::Struct => write!(f, "struct"),
            SymbolType::Enum => write!(f, "enum"),
            SymbolType::Constant => write!(f, "constant"),
            SymbolType::Variable => write!(f, "variable"),
            SymbolType::Other => write!(f, "other"),
        }
    }
}

impl SymbolType {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "function" | "func" => SymbolType::Function,
            "method" => SymbolType::Method,
            "type" => SymbolType::Type,
            "interface" => SymbolType::Interface,
            "class" => SymbolType::Class,
            "struct" => SymbolType::Struct,
            "enum" => SymbolType::Enum,
            "constant" | "const" => SymbolType::Constant,
            "variable" | "var" => SymbolType::Variable,
            _ => SymbolType::Other,
        }
    }
}

impl std::str::FromStr for SymbolType {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "function" | "func" => SymbolType::Function,
            "method" => SymbolType::Method,
            "type" => SymbolType::Type,
            "interface" => SymbolType::Interface,
            "class" => SymbolType::Class,
            "struct" => SymbolType::Struct,
            "enum" => SymbolType::Enum,
            "constant" | "const" => SymbolType::Constant,
            "variable" | "var" => SymbolType::Variable,
            _ => SymbolType::Other,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scope {
    Public,
    Private,
    Exported,
    Unexported,
    Protected,
    Internal,
    #[serde(other)]
    Other,
}

impl fmt::Display for Scope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Scope::Public => write!(f, "public"),
            Scope::Private => write!(f, "private"),
            Scope::Exported => write!(f, "exported"),
            Scope::Unexported => write!(f, "unexported"),
            Scope::Protected => write!(f, "protected"),
            Scope::Internal => write!(f, "internal"),
            Scope::Other => write!(f, "other"),
        }
    }
}

impl Scope {
    pub fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "public" | "pub" => Scope::Public,
            "private" | "pub(crate)" | "pub(super)" => Scope::Private,
            "exported" => Scope::Exported,
            "unexported" => Scope::Unexported,
            "protected" => Scope::Protected,
            "internal" | "pub(in" => Scope::Internal,
            _ => Scope::Other,
        }
    }
}

impl std::str::FromStr for Scope {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "public" | "pub" => Scope::Public,
            "private" | "pub(crate)" | "pub(super)" => Scope::Private,
            "exported" => Scope::Exported,
            "unexported" => Scope::Unexported,
            "protected" => Scope::Protected,
            "internal" | "pub(in" => Scope::Internal,
            _ => Scope::Other,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_task_status_serialization() {
        let status = TaskStatus::InProgress;
        let json = serde_json::to_string(&status).unwrap();
        assert_eq!(json, "\"in_progress\"");

        let deserialized: TaskStatus = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, TaskStatus::InProgress);
    }

    #[test]
    fn test_priority_ordering() {
        assert!(Priority::P0 < Priority::P1);
        assert!(Priority::P1 < Priority::P2);
        assert!(Priority::P2 < Priority::P3);
    }

    #[test]
    fn test_edge_class_serialization() {
        let edge_class = EdgeClass::Structural;
        let json = serde_json::to_string(&edge_class).unwrap();
        assert_eq!(json, "\"STRUCTURAL\"");

        let deserialized: EdgeClass = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized, EdgeClass::Structural);
    }

    #[test]
    fn test_session_strategy_default() {
        let default_strategy = SessionStrategy::default();
        assert_eq!(default_strategy, SessionStrategy::Latest);
    }

    #[test]
    fn test_doc_type_other_variant() {
        let json = "\"custom_type\"";
        let doc_type: DocType = serde_json::from_str(json).unwrap();
        assert_eq!(doc_type, DocType::Other);
    }

    #[test]
    fn test_enum_values_task_status() {
        let values = TaskStatus::valid_values();
        assert_eq!(values.len(), 5);
        assert!(values.contains(&"todo".to_string()));
        assert!(values.contains(&"in_progress".to_string()));
        assert!(values.contains(&"blocked".to_string()));
        assert!(values.contains(&"done".to_string()));
        assert!(values.contains(&"cancelled".to_string()));
    }

    #[test]
    fn test_enum_values_plan_status() {
        let values = PlanStatus::valid_values();
        assert_eq!(values.len(), 3);
        assert!(values.contains(&"draft".to_string()));
        assert!(values.contains(&"in_progress".to_string()));
        assert!(values.contains(&"completed".to_string()));
    }

    #[test]
    fn test_invalid_task_status_deserialization() {
        let json = "\"completed\""; // Invalid - should be "done"
        let result: Result<TaskStatus, _> = serde_json::from_str(json);
        assert!(result.is_err());
    }
}
