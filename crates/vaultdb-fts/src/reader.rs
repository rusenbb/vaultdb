//! FTS5 query path. One function, deliberately small surface.

use std::collections::HashSet;

use rusqlite::Connection;

use super::FtsError;

/// One FTS5 hit: the document summary plus a snippet of the matching
/// body region (with `<<` / `>>` markers around the matched terms).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    pub file_id: String,
    pub name: String,
    pub snippet: String,
    /// Tags as stored in the FTS5 row, split on whitespace. Consumers
    /// use these to derive type tags (e.g. eduport's
    /// `eduport-type/<value>`) or filter further.
    pub tags: Vec<String>,
}

/// Run an FTS5 `MATCH` query and return up to `limit` hits, optionally
/// intersected with a tag filter (a document must carry **every** tag
/// in `filter_tags` to be returned).
///
/// `query` is passed straight to FTS5. Callers escape FTS5-special
/// characters themselves. Hits come back in FTS5 default rank order.
pub fn search(
    conn: &Connection,
    query: &str,
    limit: usize,
    filter_tags: &[&str],
) -> Result<Vec<SearchHit>, FtsError> {
    // Without a tag filter the SQL LIMIT bounds the work; with one we
    // overscan so the Rust-side intersection has candidates to satisfy
    // `limit` after the filter.
    let scan_limit = if filter_tags.is_empty() {
        limit
    } else {
        limit * 4
    };

    let mut stmt = conn.prepare(
        "SELECT e.file_id, e.name, \
                snippet(entities_fts, 0, '<<', '>>', '...', 16) AS snippet, \
                entities_fts.tags AS row_tags \
         FROM entities_fts \
         JOIN entities e ON e.rowid = entities_fts.rowid \
         WHERE entities_fts MATCH ?1 \
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(
        rusqlite::params![query, scan_limit as i64],
        |r| -> rusqlite::Result<(String, String, String, String)> {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        },
    )?;

    let required: HashSet<&str> = filter_tags.iter().copied().collect();
    let mut out: Vec<SearchHit> = Vec::new();
    for row in rows {
        let (file_id, name, snippet, row_tags) = row?;
        let tag_list: Vec<String> = row_tags.split_whitespace().map(String::from).collect();
        if !required.is_empty() {
            let row_set: HashSet<&str> = row_tags.split_whitespace().collect();
            if !required.iter().all(|t| row_set.contains(t)) {
                continue;
            }
        }
        out.push(SearchHit {
            file_id,
            name,
            snippet,
            tags: tag_list,
        });
        if out.len() >= limit {
            break;
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::{Document, FtsIndex, upsert};
    use super::*;
    use std::path::Path;

    fn upsert_doc(index: &FtsIndex, file_id: &str, name: &str, body: &str, tags: &[&str]) {
        let tags_owned: Vec<String> = tags.iter().map(|s| s.to_string()).collect();
        upsert(
            index.conn(),
            &Document {
                file_id,
                path: Path::new(&format!("{file_id}.md")),
                mtime_ns: 0,
                body,
                name,
                tags: &tags_owned,
                custom_text: "",
            },
        )
        .unwrap();
    }

    #[test]
    fn search_finds_body_match() {
        let index = FtsIndex::open_in_memory().unwrap();
        upsert_doc(&index, "n1", "Alpha", "the quick brown fox", &[]);
        upsert_doc(&index, "n2", "Beta", "lazy dog", &[]);
        let hits = search(index.conn(), "fox", 10, &[]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Alpha");
        assert!(hits[0].snippet.contains("fox"));
    }

    #[test]
    fn search_intersects_with_tag_filter() {
        let index = FtsIndex::open_in_memory().unwrap();
        upsert_doc(&index, "n1", "Alpha", "match", &["draft"]);
        upsert_doc(&index, "n2", "Beta", "match", &["published"]);
        let hits = search(index.conn(), "match", 10, &["draft"]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Alpha");
    }

    #[test]
    fn search_requires_all_tags() {
        let index = FtsIndex::open_in_memory().unwrap();
        upsert_doc(&index, "a", "Alpha", "match", &["draft", "japan"]);
        upsert_doc(&index, "b", "Beta", "match", &["draft"]);
        let hits = search(index.conn(), "match", 10, &["draft", "japan"]).unwrap();
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].name, "Alpha");
    }
}
