//! Crash-recovery journal for [`crate::RenameBuilder::execute`].
//!
//! ## Why this exists
//!
//! `RenameBuilder` performs two operations: (1) rename the source `.md`
//! file, and (2) rewrite every `[[wikilink]]` pointing at the old name
//! across the vault. If the process crashes between (1) and (2), the
//! vault is left in an inconsistent state — the file has been renamed
//! but other notes still reference the old name.
//!
//! The journal makes this recoverable: before any disk change happens,
//! we write a journal file describing what we're about to do. On the
//! next mutation (or via an explicit [`crate::Vault::recover`] call),
//! pending journals are replayed: each is idempotent, so partial
//! progress is fine.
//!
//! ## Layout
//!
//! Journals live at `<vault>/.vaultdb/rename-journal/<timestamp>.json`.
//! The timestamp prefix gives chronological ordering when multiple
//! journals exist (e.g. several renames committed close together
//! before a recovery sweep).
//!
//! ## Replay state machine
//!
//! Three observable states when a journal is loaded:
//!
//! | source path | dest path | meaning                         | action                                  |
//! |-------------|-----------|---------------------------------|-----------------------------------------|
//! | exists      | missing   | rename never ran                | do the rename, then rewrite backlinks   |
//! | missing     | exists    | rename ran, rewrites incomplete | rewrite backlinks (idempotent)          |
//! | missing     | missing   | something deleted both          | log + delete journal (stale)            |
//! | exists      | exists    | conflict introduced externally  | log + skip rename, rewrite backlinks    |
//!
//! Backlink rewrites are themselves idempotent: rewriting a file whose
//! `[[from]]` references have already been replaced by `[[to]]` is a
//! no-op (the replace finds no matches and returns identical content).

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{Result, VaultdbError};
use crate::writer;

/// Subdirectory under `.vaultdb/` where rename journals are written.
pub(crate) const JOURNAL_SUBDIR: &str = "rename-journal";

/// On-disk record describing one in-progress rename.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RenameJournal {
    /// Path the source `.md` file lived at before the rename.
    pub source: PathBuf,
    /// Path the source file is being moved to.
    pub dest: PathBuf,
    /// Name (filename without `.md`) used in `[[wikilinks]]` to identify
    /// this record before the rename.
    pub from_name: String,
    /// Name being introduced.
    pub to_name: String,
    /// Files known to contain at least one `[[from_name]]` reference
    /// when the journal was written. May contain extras that no longer
    /// match (replay handles that gracefully).
    pub backlinks: Vec<PathBuf>,
}

fn journal_dir(vault_root: &Path) -> PathBuf {
    vault_root.join(crate::lock::META_DIR).join(JOURNAL_SUBDIR)
}

/// Atomically write `journal` to a new file under `<vault>/.vaultdb/rename-journal/`.
/// Returns the path of the written journal so the caller can delete it
/// after the rename + rewrites complete successfully.
pub(crate) fn write(vault_root: &Path, journal: &RenameJournal) -> Result<PathBuf> {
    let dir = journal_dir(vault_root);
    std::fs::create_dir_all(&dir).map_err(VaultdbError::Io)?;

    // Filename = nanoseconds-since-epoch. This gives chronological
    // ordering and avoids collisions even under tight back-to-back writes.
    // We deliberately don't use a hash of contents: two functionally
    // identical renames a millisecond apart should still produce two
    // distinct journals.
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let path = dir.join(format!("{:032}.json", stamp));
    let serialized = serde_json::to_string_pretty(journal)
        .map_err(|e| VaultdbError::Internal(format!("serialize rename journal: {}", e)))?;
    writer::atomic_write(&path, &serialized)?;
    Ok(path)
}

/// Best-effort delete of a journal file. Errors are converted to `Ok(())`
/// in the success path because by the time we're deleting, the rename and
/// every backlink rewrite have already landed; a delete failure here only
/// means we'll do a redundant idempotent replay on next startup.
pub(crate) fn delete(journal_path: &Path) {
    let _ = std::fs::remove_file(journal_path);
}

/// List every pending journal under `<vault>/.vaultdb/rename-journal/`,
/// sorted by filename (chronological because of the timestamp prefix).
pub fn list_pending(vault_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = journal_dir(vault_root);
    if !dir.is_dir() {
        return Ok(Vec::new());
    }
    let mut paths: Vec<PathBuf> = Vec::new();
    for entry in std::fs::read_dir(&dir).map_err(VaultdbError::Io)? {
        let entry = entry.map_err(VaultdbError::Io)?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) == Some("json") {
            paths.push(path);
        }
    }
    paths.sort();
    Ok(paths)
}

/// Replay one journal: do the rename if it's still pending, then make
/// every backlink rewrite idempotent. On success the journal file is
/// deleted; on error the journal stays for the next replay sweep.
pub fn replay(journal_path: &Path) -> Result<()> {
    let raw = std::fs::read_to_string(journal_path).map_err(VaultdbError::Io)?;
    let journal: RenameJournal = serde_json::from_str(&raw).map_err(|e| {
        VaultdbError::Internal(format!(
            "parse rename journal {}: {}",
            journal_path.display(),
            e
        ))
    })?;

    let source_exists = journal.source.is_file();
    let dest_exists = journal.dest.is_file();

    match (source_exists, dest_exists) {
        (true, false) => {
            // State A: rename never ran. Do it now.
            std::fs::rename(&journal.source, &journal.dest).map_err(VaultdbError::Io)?;
        }
        (false, true) => {
            // State B: rename ran, backlink rewrites may be incomplete.
            // Continue to the rewrite loop below.
        }
        (false, false) => {
            // State C: stale journal — both files gone. Nothing to do.
            // Drop the journal and return.
            delete(journal_path);
            return Ok(());
        }
        (true, true) => {
            // State D: source still exists AND dest already exists. The
            // rename can't proceed (target conflict), but if dest is the
            // post-rename file from a prior partial run, backlinks may
            // still need rewriting. Skip the rename, run the rewrite
            // loop. This is rare (would require an external user
            // creating a file at the dest path mid-rename); we err on
            // the side of finishing the partial rename's tail rather
            // than aborting.
        }
    }

    let mut last_err: Option<VaultdbError> = None;
    for backlink in &journal.backlinks {
        if !backlink.is_file() {
            continue; // file was deleted externally; skip
        }
        let content = match std::fs::read_to_string(backlink) {
            Ok(c) => c,
            Err(e) => {
                last_err = Some(VaultdbError::Io(e));
                continue;
            }
        };
        let new_content =
            crate::mutation::rewrite_wikilinks(&content, &journal.from_name, &journal.to_name);
        if new_content == content {
            // Already rewritten (or never matched); idempotent no-op.
            continue;
        }
        if let Err(e) = writer::atomic_write(backlink, &new_content) {
            last_err = Some(VaultdbError::Io(e));
        }
    }

    // Only delete the journal if every rewrite succeeded. If anything
    // failed, leave the journal so the next sweep retries those files.
    if let Some(err) = last_err {
        return Err(err);
    }
    delete(journal_path);
    Ok(())
}

/// Replay every pending journal. Returns the number of journals
/// processed (whether successfully replayed and deleted, or processed
/// as stale and deleted).
pub fn replay_all(vault_root: &Path) -> Result<usize> {
    let pending = list_pending(vault_root)?;
    let mut count = 0;
    for path in pending {
        replay(&path)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        dir
    }

    fn write_md(path: &Path, content: &str) {
        fs::write(path, content).unwrap();
    }

    #[test]
    fn write_and_list_journal() {
        let dir = make_vault();
        let journal = RenameJournal {
            source: dir.path().join("notes/old.md"),
            dest: dir.path().join("notes/new.md"),
            from_name: "old".into(),
            to_name: "new".into(),
            backlinks: vec![dir.path().join("notes/other.md")],
        };
        let path = write(dir.path(), &journal).unwrap();
        assert!(path.is_file());

        let pending = list_pending(dir.path()).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0], path);
    }

    #[test]
    fn replay_state_a_renames_then_rewrites() {
        // State A: source exists, dest missing. Replay does the rename
        // and the rewrite.
        let dir = make_vault();
        let source = dir.path().join("notes/Stanford.md");
        let dest = dir.path().join("notes/Stanford University.md");
        let other = dir.path().join("notes/Application.md");

        write_md(&source, "---\nstatus: active\n---\nMain note.\n");
        write_md(
            &other,
            "---\nrelated:\n  - \"[[Stanford]]\"\n---\nApplied to [[Stanford]] last week.\n",
        );

        let journal = RenameJournal {
            source: source.clone(),
            dest: dest.clone(),
            from_name: "Stanford".into(),
            to_name: "Stanford University".into(),
            backlinks: vec![other.clone()],
        };
        let journal_path = write(dir.path(), &journal).unwrap();

        replay(&journal_path).unwrap();

        // Rename done.
        assert!(!source.exists());
        assert!(dest.is_file());

        // Backlinks rewritten.
        let other_content = fs::read_to_string(&other).unwrap();
        assert!(other_content.contains("[[Stanford University]]"));
        assert!(!other_content.contains("[[Stanford]]"));

        // Journal cleaned up.
        assert!(!journal_path.exists());
    }

    #[test]
    fn replay_state_b_finishes_partial_backlink_rewrites() {
        // State B: rename completed, some backlinks rewritten, one still
        // pointing at the old name.
        let dir = make_vault();
        let source = dir.path().join("notes/Stanford.md");
        let dest = dir.path().join("notes/Stanford University.md");
        let already = dir.path().join("notes/AlreadyRewritten.md");
        let pending = dir.path().join("notes/StillOldName.md");

        // Rename has already happened (source missing, dest present).
        write_md(&dest, "---\n---\nMain note.\n");
        write_md(&already, "Sees [[Stanford University]] only.\n");
        write_md(&pending, "Sees [[Stanford]] and needs rewriting.\n");

        let journal = RenameJournal {
            source,
            dest: dest.clone(),
            from_name: "Stanford".into(),
            to_name: "Stanford University".into(),
            backlinks: vec![already.clone(), pending.clone()],
        };
        let journal_path = write(dir.path(), &journal).unwrap();

        replay(&journal_path).unwrap();

        // The already-rewritten file is unchanged.
        let a = fs::read_to_string(&already).unwrap();
        assert_eq!(a, "Sees [[Stanford University]] only.\n");

        // The pending file is rewritten.
        let p = fs::read_to_string(&pending).unwrap();
        assert!(p.contains("[[Stanford University]]"));
        assert!(!p.contains("[[Stanford]] "));

        // Journal cleaned up.
        assert!(!journal_path.exists());
    }

    #[test]
    fn replay_state_c_stale_journal_is_cleaned() {
        // State C: both source and dest gone (e.g. user manually deleted
        // both files). Replay should delete the journal and not error.
        let dir = make_vault();
        let journal = RenameJournal {
            source: dir.path().join("notes/Gone.md"),
            dest: dir.path().join("notes/AlsoGone.md"),
            from_name: "Gone".into(),
            to_name: "AlsoGone".into(),
            backlinks: vec![],
        };
        let journal_path = write(dir.path(), &journal).unwrap();

        replay(&journal_path).unwrap();

        // Journal cleaned up; nothing else changed.
        assert!(!journal_path.exists());
    }

    #[test]
    fn replay_is_idempotent_when_called_twice() {
        // Calling replay_all twice in a row must not break anything.
        // After the first call, the journals are gone, so the second
        // is a no-op.
        let dir = make_vault();
        let source = dir.path().join("notes/X.md");
        let dest = dir.path().join("notes/Y.md");
        write_md(&source, "Body.\n");

        let journal = RenameJournal {
            source: source.clone(),
            dest: dest.clone(),
            from_name: "X".into(),
            to_name: "Y".into(),
            backlinks: vec![],
        };
        write(dir.path(), &journal).unwrap();

        let n1 = replay_all(dir.path()).unwrap();
        let n2 = replay_all(dir.path()).unwrap();
        assert_eq!(n1, 1);
        assert_eq!(n2, 0);
        assert!(dest.is_file());
    }

    #[test]
    fn replay_all_processes_multiple_journals_in_order() {
        let dir = make_vault();
        let a = dir.path().join("notes/A.md");
        let b = dir.path().join("notes/B.md");
        let c = dir.path().join("notes/C.md");
        write_md(&a, "Body.\n");

        // Journal 1: A -> B
        write(
            dir.path(),
            &RenameJournal {
                source: a.clone(),
                dest: b.clone(),
                from_name: "A".into(),
                to_name: "B".into(),
                backlinks: vec![],
            },
        )
        .unwrap();
        // Tiny pause to ensure distinct timestamps for journal 2.
        std::thread::sleep(std::time::Duration::from_millis(2));
        // Journal 2: B -> C (uses output of journal 1).
        write(
            dir.path(),
            &RenameJournal {
                source: b.clone(),
                dest: c.clone(),
                from_name: "B".into(),
                to_name: "C".into(),
                backlinks: vec![],
            },
        )
        .unwrap();

        let n = replay_all(dir.path()).unwrap();
        assert_eq!(n, 2);
        assert!(!a.exists());
        assert!(!b.exists());
        assert!(c.is_file(), "expected final state to be C");
    }
}
