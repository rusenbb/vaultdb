use thiserror::Error;

#[derive(Error, Debug)]
pub enum VaultdbError {
    #[error("vault not found: no .obsidian/ directory in {0} or any parent")]
    VaultNotFound(String),

    #[error("folder not found: {0}")]
    FolderNotFound(String),

    #[error("no frontmatter in file: {0}")]
    NoFrontmatter(String),

    #[error("invalid frontmatter YAML in {file}: {reason}")]
    InvalidFrontmatter { file: String, reason: String },

    #[error("invalid where expression: {0}")]
    InvalidWhereExpr(String),

    #[error("type mismatch: field '{field}' is {actual}, cannot compare as {expected}")]
    TypeMismatch {
        field: String,
        actual: String,
        expected: String,
    },

    #[error("regex error in pattern '{pattern}': {reason}")]
    RegexError { pattern: String, reason: String },

    #[error("schema error: {0}")]
    SchemaError(String),

    #[error("operation refused: {reason}")]
    SafetyRefused { reason: String },

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error(transparent)]
    Yaml(#[from] serde_yaml::Error),
}

pub type Result<T> = std::result::Result<T, VaultdbError>;
