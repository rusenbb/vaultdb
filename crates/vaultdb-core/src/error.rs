//! Error types: [`VaultdbError`] for fatal failures, [`ParseError`] for
//! non-fatal per-file diagnostics returned by [`crate::LoadResult`].

use std::path::PathBuf;
use thiserror::Error;

/// A non-fatal parse failure for a single file.
///
/// Returned in `LoadResult::parse_errors` when `Vault::load_records` encounters
/// a file with malformed frontmatter. The application layer decides whether to
/// surface, log, or ignore these.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ParseError {
    pub file: PathBuf,
    pub message: String,
}

#[derive(Error, Debug)]
#[non_exhaustive]
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

    /// Vaultdb's own internal invariant was violated (e.g. a journal file
    /// failed to round-trip through serde, a lock sentinel couldn't be
    /// created because of an unexpected filesystem state). These are bugs
    /// in vaultdb-core or unrecoverable filesystem situations, not user
    /// errors.
    #[error("internal error: {0}")]
    Internal(String),

    #[error(transparent)]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, VaultdbError>;

#[cfg(test)]
mod parse_error_tests {
    use super::ParseError;
    use std::path::PathBuf;

    #[test]
    fn parse_error_serializes() {
        let err = ParseError {
            file: PathBuf::from("foo.md"),
            message: "bad yaml".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        assert!(json.contains("foo.md"));
        assert!(json.contains("bad yaml"));
    }

    #[test]
    fn parse_error_round_trips() {
        let err = ParseError {
            file: PathBuf::from("notes/x.md"),
            message: "oops".into(),
        };
        let json = serde_json::to_string(&err).unwrap();
        let back: ParseError = serde_json::from_str(&json).unwrap();
        assert_eq!(back.file, err.file);
        assert_eq!(back.message, err.message);
    }
}
