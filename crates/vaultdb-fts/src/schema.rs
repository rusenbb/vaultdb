//! SQLite + FTS5 schema. Same approach as eduport's original
//! sidebar — version mismatches blow away the FTS5 virtual table and
//! let the caller re-populate via reconcile. The base `entities`
//! mtime-tracking table is preserved across migrations.

use rusqlite::Connection;

use super::Result;

/// Bumped whenever the on-disk schema changes shape. A mismatch with
/// `PRAGMA user_version` triggers a full rebuild of both `entities`
/// and `entities_fts` from vault state.
///
/// History
/// - **1**: initial release.
/// - **2**: dropped legacy `entities.type` / `entities.body` /
///   `entities.frontmatter` columns inherited from eduport's
///   pre-extraction schema. Legacy databases upgrade by full rebuild
///   (the index is a derived cache; rebuilds are sub-second at
///   target vault sizes).
pub const FTS_SCHEMA_VERSION: i64 = 2;

const DDL: &str = r#"
CREATE TABLE IF NOT EXISTS entities (
    file_id     TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    path        TEXT NOT NULL,
    mtime_ns    INTEGER NOT NULL
);

CREATE TABLE IF NOT EXISTS parse_errors (
    path        TEXT PRIMARY KEY,
    message     TEXT NOT NULL,
    occurred_at TEXT NOT NULL DEFAULT (datetime('now'))
);

CREATE VIRTUAL TABLE IF NOT EXISTS entities_fts USING fts5(
    body,
    name,
    tags,
    custom_text,
    tokenize="unicode61 remove_diacritics 2"
);
"#;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InitOutcome {
    /// `true` iff the FTS5 virtual table was dropped during init.
    /// Callers re-populate via `reconcile`.
    pub fts_rebuilt: bool,
}

pub fn init_schema(conn: &Connection) -> Result<InitOutcome> {
    let current_version: i64 = conn
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap_or(0);

    let mut fts_rebuilt = false;
    if current_version != 0 && current_version != FTS_SCHEMA_VERSION {
        // Legacy schemas (e.g. eduport's pre-extraction version that
        // carried `entities.type`/`body`/`frontmatter` NOT NULL
        // columns) need a full rebuild — the new code doesn't write
        // those columns, so retaining the table would NOT-NULL-crash
        // on first upsert. Both `entities` and `entities_fts` get
        // dropped here; the caller (consumer) re-populates via
        // `reconcile`.
        conn.execute("DROP TABLE IF EXISTS entities_fts", [])?;
        conn.execute("DROP TABLE IF EXISTS entities", [])?;
        fts_rebuilt = true;
    }

    conn.execute_batch(DDL)?;
    conn.pragma_update(None, "user_version", FTS_SCHEMA_VERSION)?;
    Ok(InitOutcome { fts_rebuilt })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fresh_database_initialises_cleanly() {
        let conn = Connection::open_in_memory().unwrap();
        let out = init_schema(&conn).unwrap();
        assert!(!out.fts_rebuilt);
        let v: i64 = conn
            .pragma_query_value(None, "user_version", |r| r.get(0))
            .unwrap();
        assert_eq!(v, FTS_SCHEMA_VERSION);
    }

    #[test]
    fn init_schema_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        init_schema(&conn).unwrap();
    }

    #[test]
    fn version_mismatch_drops_fts_table() {
        let conn = Connection::open_in_memory().unwrap();
        init_schema(&conn).unwrap();
        conn.pragma_update(None, "user_version", 999_i64).unwrap();
        let out = init_schema(&conn).unwrap();
        assert!(out.fts_rebuilt);
    }
}
