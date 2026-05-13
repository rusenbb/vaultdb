//! Mutators for the FTS index — `upsert` / `delete`. Free functions
//! over `&Connection` so callers can batch them inside their own
//! transaction (e.g. a watcher's debounced batch).

use std::path::Path;

use rusqlite::{Connection, params};

use super::FtsError;

/// One document to be inserted into (or updated in) the index. Borrows
/// to avoid an alloc per upsert in hot paths.
#[derive(Debug, Clone, Copy)]
pub struct Document<'a> {
    pub file_id: &'a str,
    pub path: &'a Path,
    pub mtime_ns: i64,
    pub body: &'a str,
    pub name: &'a str,
    pub tags: &'a [String],
    /// Concatenated string of consumer-chosen prose to feed into the
    /// FTS5 `custom_text` column. Eduport stuffs in user-declared
    /// text/url custom-property values here; other consumers can leave
    /// it empty.
    pub custom_text: &'a str,
}

/// Update or insert the index row for one document. Wraps itself in a
/// transaction if the connection is currently autocommit; otherwise
/// participates in the outer transaction.
pub fn upsert(conn: &Connection, doc: &Document) -> Result<(), FtsError> {
    with_optional_tx(conn, |conn| {
        conn.execute(
            "INSERT OR REPLACE INTO entities (file_id, name, path, mtime_ns) \
             VALUES (?1, ?2, ?3, ?4)",
            params![
                doc.file_id,
                doc.name,
                doc.path.to_string_lossy().as_ref(),
                doc.mtime_ns
            ],
        )?;
        let rowid: i64 = conn.query_row(
            "SELECT rowid FROM entities WHERE file_id = ?1",
            params![doc.file_id],
            |r| r.get(0),
        )?;
        // Explicit delete-then-insert: FTS5 virtual tables don't compose
        // cleanly with INSERT OR REPLACE on contentless rows.
        conn.execute("DELETE FROM entities_fts WHERE rowid = ?1", params![rowid])?;
        let tags_joined = doc.tags.join(" ");
        conn.execute(
            "INSERT INTO entities_fts(rowid, body, name, tags, custom_text) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![rowid, doc.body, doc.name, tags_joined, doc.custom_text],
        )?;
        Ok(())
    })
}

/// Remove a document and its FTS row from the index.
pub fn delete(conn: &Connection, file_id: &str) -> Result<(), FtsError> {
    with_optional_tx(conn, |conn| {
        let rowid: Option<i64> = conn
            .query_row(
                "SELECT rowid FROM entities WHERE file_id = ?1",
                params![file_id],
                |r| r.get(0),
            )
            .ok();
        if let Some(rowid) = rowid {
            conn.execute("DELETE FROM entities_fts WHERE rowid = ?1", params![rowid])?;
        }
        conn.execute("DELETE FROM entities WHERE file_id = ?1", params![file_id])?;
        Ok(())
    })
}

fn with_optional_tx<F>(conn: &Connection, f: F) -> Result<(), FtsError>
where
    F: FnOnce(&Connection) -> Result<(), FtsError>,
{
    let owns_tx = conn.is_autocommit();
    if owns_tx {
        conn.execute("BEGIN IMMEDIATE", [])?;
    }
    let result = f(conn);
    if owns_tx {
        match &result {
            Ok(()) => {
                conn.execute("COMMIT", [])?;
            }
            Err(_) => {
                let _ = conn.execute("ROLLBACK", []);
            }
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::super::FtsIndex;
    use super::*;

    #[test]
    fn upsert_then_delete_round_trip() {
        let index = FtsIndex::open_in_memory().unwrap();
        let tags = vec!["greeting".to_string()];
        let path = Path::new("hello.md");
        upsert(
            index.conn(),
            &Document {
                file_id: "hello",
                path,
                mtime_ns: 1,
                body: "world",
                name: "Hello",
                tags: &tags,
                custom_text: "",
            },
        )
        .unwrap();
        let count: i64 = index
            .conn()
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 1);

        delete(index.conn(), "hello").unwrap();
        let count: i64 = index
            .conn()
            .query_row("SELECT COUNT(*) FROM entities", [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0);
        let fts_count: i64 = index
            .conn()
            .query_row("SELECT COUNT(*) FROM entities_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts_count, 0);
    }
}
