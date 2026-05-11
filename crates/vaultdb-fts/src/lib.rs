//! # vaultdb-fts
//!
//! Opt-in full-text search index for vaultdb vaults. SQLite + FTS5
//! under the hood; same shape vaultdb-core was deliberately not
//! built with.
//!
//! ## When to use this
//!
//! vaultdb-core's defining choice — "no daemon, no cache, no state
//! files" — keeps the engine small and predictable, but means there
//! is no built-in `WHERE body MATCH "fox"` operator. Consumers who
//! need real full-text search add `vaultdb-fts` as a separate
//! dependency and own the index's lifecycle.
//!
//! ## What this crate is
//!
//! - A persistent SQLite database (or in-memory for tests)
//! - An FTS5 virtual table indexed on body / name / tags / custom_text
//! - A reconcile path that brings the index in agreement with a
//!   `Vault`'s current on-disk state
//! - Free mutator functions (`upsert`, `delete`) so consumers can
//!   batch updates inside their own transaction
//! - A tiny `parse_errors` table for vault files that wouldn't parse
//!   (the canonical use case is surfacing them in a Status page)
//!
//! ## What this crate is NOT
//!
//! - A watcher. Consumers wire vaultdb-core's watcher (or any other
//!   filesystem event source) and forward events into `upsert` /
//!   `delete`.
//! - A schema enforcer. The document shape is bytes-in; consumers
//!   decide what's body, what's a tag, what counts as searchable
//!   custom text.
//! - A query planner. `search` accepts an FTS5 MATCH expression and
//!   passes it through. Callers escape FTS5-special characters
//!   themselves.

mod parse_errors;
mod reader;
mod reconcile;
mod schema;
mod writer;

pub use parse_errors::{
    ParseErrorRow, clear_parse_error, list_parse_errors, record_parse_error,
};
pub use reader::{SearchHit, search};
pub use reconcile::{OwnedDocument, ReconcileSummary, reconcile};
pub use schema::{FTS_SCHEMA_VERSION, InitOutcome, init_schema};
pub use writer::{Document, delete, upsert};

use rusqlite::Connection;
use std::path::Path;

/// Convenience wrapper that owns a [`Connection`] with the schema
/// initialised. Use [`FtsIndex::conn`] / [`FtsIndex::conn_mut`] when
/// you need to drop down to the free-function API for transaction
/// composition.
pub struct FtsIndex {
    conn: Connection,
}

impl FtsIndex {
    /// Open or create the index database at `path`. The parent
    /// directory must already exist; this matches vaultdb-core's
    /// convention of "the caller decides where things live".
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        let _ = init_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Open an in-memory index. Used in tests and ephemeral
    /// reconcile flows that want a fresh index per call.
    pub fn open_in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute("PRAGMA foreign_keys = ON", [])?;
        let _ = init_schema(&conn)?;
        Ok(Self { conn })
    }

    /// Borrow the underlying connection. Use this to compose multiple
    /// writer/reader calls in a single transaction.
    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    /// Mutable borrow. Required for `conn.transaction()`.
    pub fn conn_mut(&mut self) -> &mut Connection {
        &mut self.conn
    }
}

#[derive(Debug, thiserror::Error)]
pub enum FtsError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),

    #[error(transparent)]
    Vault(#[from] vaultdb_core::VaultdbError),

    #[error("data error: {0}")]
    Data(String),

    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, FtsError>;
