//! [`OrmError`]: the public error type for `vaultdb-orm`.
//!
//! Wraps `vaultdb_core::VaultdbError` and `serde_json::Error` so consumers
//! get a single error to match on regardless of where in the typed pipeline
//! the failure occurred.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum OrmError {
    /// Underlying vault operation failed.
    #[error("vault error: {0}")]
    Vault(#[from] vaultdb_core::VaultdbError),

    /// Frontmatter could not be deserialised into the typed struct.
    #[error("could not deserialise record into typed struct: {0}")]
    Deserialize(#[from] serde_json::Error),

    /// A typed struct could not be turned back into frontmatter values
    /// (e.g. an unrepresentable number).
    #[error("ORM error: {0}")]
    Custom(String),
}

pub type Result<T> = std::result::Result<T, OrmError>;
