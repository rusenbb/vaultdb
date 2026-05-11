//! Reconcile path — walk a `Vault`, project each `Record` to a
//! `Document` via a caller-supplied closure, and bring the index
//! into agreement with on-disk state.
//!
//! Reconcile is the cold-start path and the recovery path after
//! anything bypasses the watcher (sync programs, manual `cp`, backup
//! restore). Steady-state incremental updates go through
//! `writer::upsert` / `writer::delete`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use rusqlite::Connection;
use vaultdb_core::{Record, Vault};

use super::FtsError;
use super::writer::{Document, delete, upsert};

/// Owned document a `doc_for` projection returns. Owned to keep the
/// projection closure's signature simple — reconcile is not hot.
#[derive(Debug, Clone)]
pub struct OwnedDocument {
    pub file_id: String,
    pub path: PathBuf,
    pub mtime_ns: i64,
    pub body: String,
    pub name: String,
    pub tags: Vec<String>,
    pub custom_text: String,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileSummary {
    pub added: usize,
    pub updated: usize,
    pub removed: usize,
    pub unchanged: usize,
    /// Records the projection returned `None` for. Consumers might
    /// also bump their own parse-error counter via
    /// `record_parse_error`.
    pub skipped: usize,
}

/// Walk the vault, project each record via `doc_for`, and bring the
/// index in agreement.
///
/// `doc_for` returns `None` to skip a record (e.g. a file whose
/// frontmatter doesn't match the consumer's domain shape — eduport
/// uses this to drop non-entity files).
///
/// Files are matched against the index by `file_id`. Stale rows
/// (file gone from vault) are removed. mtime-keyed fast path skips
/// re-upserting unchanged documents.
pub fn reconcile<F>(
    conn: &Connection,
    vault: &Vault,
    mut doc_for: F,
) -> Result<ReconcileSummary, FtsError>
where
    F: FnMut(&Record) -> Option<OwnedDocument>,
{
    let mut summary = ReconcileSummary::default();

    // Snapshot indexed (file_id, mtime) so we can detect deletions in
    // O(1) per file.
    let existing: HashMap<String, i64> = {
        let mut stmt = conn.prepare("SELECT file_id, mtime_ns FROM entities")?;
        stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?)))?
            .collect::<rusqlite::Result<HashMap<_, _>>>()?
    };

    let mut seen: HashSet<String> = HashSet::new();
    // Need `raw_content` populated so the projection closure can
    // extract the body cheaply. `load_records_with_content` preserves
    // it; `load_records` doesn't.
    let result = vault
        .load_records_with_content(&vault.root, false, false)
        .map_err(|e| FtsError::Data(format!("vault.load_records_with_content failed: {e}")))?;

    for record in &result.records {
        let Some(doc) = doc_for(record) else {
            summary.skipped += 1;
            continue;
        };
        seen.insert(doc.file_id.clone());

        if existing.get(&doc.file_id) == Some(&doc.mtime_ns) {
            summary.unchanged += 1;
            continue;
        }

        let was_existing = existing.contains_key(&doc.file_id);
        upsert(
            conn,
            &Document {
                file_id: &doc.file_id,
                path: &doc.path,
                mtime_ns: doc.mtime_ns,
                body: &doc.body,
                name: &doc.name,
                tags: &doc.tags,
                custom_text: &doc.custom_text,
            },
        )?;
        if was_existing {
            summary.updated += 1;
        } else {
            summary.added += 1;
        }
    }

    for file_id in existing.keys() {
        if !seen.contains(file_id) {
            delete(conn, file_id)?;
            summary.removed += 1;
        }
    }

    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::super::FtsIndex;
    use super::*;
    use std::fs;
    use std::time::UNIX_EPOCH;
    use tempfile::TempDir;

    fn write_note(root: &std::path::Path, stem: &str, name: &str, body: &str) {
        fs::write(
            root.join(format!("{stem}.md")),
            format!("---\nname: {name}\ntags:\n  - kind/note\n---\n{body}"),
        )
        .unwrap();
    }

    fn project(record: &Record) -> Option<OwnedDocument> {
        let file_id = record.path.file_stem()?.to_str()?.to_string();
        let name = match record.fields.get("name") {
            Some(vaultdb_core::Value::String(s)) => s.clone(),
            _ => return None,
        };
        let tags = match record.fields.get("tags") {
            Some(vaultdb_core::Value::List(items)) => items
                .iter()
                .filter_map(|v| match v {
                    vaultdb_core::Value::String(s) => Some(s.clone()),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        };
        let mtime_ns = record
            .path
            .metadata()
            .ok()
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| d.as_nanos() as i64)
            .unwrap_or(0);
        let body = body_from_raw(record.raw_content.as_deref().unwrap_or(""));
        Some(OwnedDocument {
            file_id,
            path: record.path.clone(),
            mtime_ns,
            body,
            name,
            tags,
            custom_text: String::new(),
        })
    }

    fn body_from_raw(raw: &str) -> String {
        // Strip the `---\nYAML\n---\n` head if present; everything
        // after is body.
        let Some(rest) = raw.strip_prefix("---\n") else {
            return raw.to_string();
        };
        match rest.find("\n---\n") {
            Some(idx) => rest[idx + "\n---\n".len()..].to_string(),
            None => raw.to_string(),
        }
    }

    fn setup_vault() -> (TempDir, Vault) {
        let tmp = TempDir::new().unwrap();
        // `.obsidian` marker so Vault::discover would also work; not
        // strictly required for `with_root`.
        fs::create_dir_all(tmp.path().join(".obsidian")).unwrap();
        let vault = Vault::with_root(tmp.path().to_path_buf());
        (tmp, vault)
    }

    #[test]
    fn reconcile_picks_up_new_file() {
        let (tmp, vault) = setup_vault();
        write_note(tmp.path(), "hello", "Hello", "world");
        let index = FtsIndex::open_in_memory().unwrap();
        let summary = reconcile(index.conn(), &vault, project).unwrap();
        assert_eq!(summary.added, 1);
        assert_eq!(summary.removed, 0);

        // Second pass: all unchanged.
        let summary = reconcile(index.conn(), &vault, project).unwrap();
        assert_eq!(summary.unchanged, 1);
        assert_eq!(summary.added, 0);
    }

    #[test]
    fn reconcile_removes_deleted_file() {
        let (tmp, vault) = setup_vault();
        write_note(tmp.path(), "gone", "Gone", "");
        let index = FtsIndex::open_in_memory().unwrap();
        reconcile(index.conn(), &vault, project).unwrap();
        fs::remove_file(tmp.path().join("gone.md")).unwrap();
        let summary = reconcile(index.conn(), &vault, project).unwrap();
        assert_eq!(summary.removed, 1);
    }

    #[test]
    fn reconcile_respects_projection_none() {
        let (tmp, vault) = setup_vault();
        // Missing `name` field — projection returns None, skipped.
        fs::write(
            tmp.path().join("skip.md"),
            "---\ntags:\n  - kind/note\n---\nbody",
        )
        .unwrap();
        let index = FtsIndex::open_in_memory().unwrap();
        let summary = reconcile(index.conn(), &vault, project).unwrap();
        assert_eq!(summary.skipped, 1);
        assert_eq!(summary.added, 0);
    }
}
