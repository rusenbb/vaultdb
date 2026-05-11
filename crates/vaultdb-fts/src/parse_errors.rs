//! Companion table for "this file's frontmatter wouldn't parse".
//! Optional — consumers who don't surface parse errors in their UI
//! can ignore these functions; the table is always present in the
//! schema because dropping it for one consumer's preference would
//! force a schema migration.

use rusqlite::{Connection, params};

use super::FtsError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseErrorRow {
    pub path: String,
    pub message: String,
    pub occurred_at: String,
}

pub fn record_parse_error(conn: &Connection, path: &str, message: &str) -> Result<(), FtsError> {
    conn.execute(
        "INSERT OR REPLACE INTO parse_errors(path, message) VALUES (?1, ?2)",
        params![path, message],
    )?;
    Ok(())
}

pub fn clear_parse_error(conn: &Connection, path: &str) -> Result<(), FtsError> {
    conn.execute("DELETE FROM parse_errors WHERE path = ?1", params![path])?;
    Ok(())
}

pub fn list_parse_errors(conn: &Connection) -> Result<Vec<ParseErrorRow>, FtsError> {
    let mut stmt =
        conn.prepare("SELECT path, message, occurred_at FROM parse_errors ORDER BY occurred_at")?;
    let rows = stmt.query_map([], |r| {
        Ok(ParseErrorRow {
            path: r.get(0)?,
            message: r.get(1)?,
            occurred_at: r.get(2)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::FtsIndex;
    use super::*;

    #[test]
    fn record_and_clear() {
        let index = FtsIndex::open_in_memory().unwrap();
        record_parse_error(index.conn(), "/tmp/bad.md", "bad frontmatter").unwrap();
        let rows = list_parse_errors(index.conn()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].message, "bad frontmatter");
        clear_parse_error(index.conn(), "/tmp/bad.md").unwrap();
        let rows = list_parse_errors(index.conn()).unwrap();
        assert_eq!(rows.len(), 0);
    }
}
