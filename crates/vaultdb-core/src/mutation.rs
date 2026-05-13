//! Public typed mutation API for vault edits.
//!
//! Each builder exposes two methods: `plan(&self, vault) -> Result<MutationReport>`
//! produces a read-only preview without touching disk, and
//! `execute(self, vault) -> Result<MutationReport>` applies the planned changes
//! and returns the same shape of report.
//!
//! The report shape is intentionally small — a vector of `PlannedChange` (path
//! plus a human-readable description) and a vector of `MutationError` for any
//! per-record failures. Consumers that need before/after frontmatter snapshots
//! can compute them by running their own diff against the records on disk.

use std::path::PathBuf;

use crate::error::{Result, VaultdbError};
use crate::query::Expr;
use crate::record::Value;
use crate::vault::Vault;
use crate::writer::{self, WriteResult};

/// A report of changes a builder would (or did) make.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MutationReport {
    pub changes: Vec<PlannedChange>,
    pub errors: Vec<MutationError>,
}

/// A single planned (or applied) change to one record.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct PlannedChange {
    pub path: PathBuf,
    pub description: String,
}

/// A failure to apply a single change.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MutationError {
    pub path: PathBuf,
    pub message: String,
}

// ── UpdateBuilder ──────────────────────────────────────────────────────────

/// Build an update mutation. The `filter` selects records; the chained
/// `set`/`unset`/`add_tag`/`remove_tag` calls accumulate operations applied
/// to each matching record's frontmatter.
#[derive(Debug, Clone)]
pub struct UpdateBuilder {
    filter: Expr,
    folder: String,
    set_fields: Vec<(String, Value)>,
    unset_fields: Vec<String>,
    add_tags: Vec<String>,
    remove_tags: Vec<String>,
    write_options: writer::WriteOptions,
}

impl UpdateBuilder {
    pub fn new(folder: impl Into<String>, filter: Expr) -> Self {
        Self {
            filter,
            folder: folder.into(),
            set_fields: Vec::new(),
            unset_fields: Vec::new(),
            add_tags: Vec::new(),
            remove_tags: Vec::new(),
            write_options: writer::WriteOptions::default(),
        }
    }

    pub fn set(mut self, field: impl Into<String>, value: Value) -> Self {
        self.set_fields.push((field.into(), value));
        self
    }

    pub fn unset(mut self, field: impl Into<String>) -> Self {
        self.unset_fields.push(field.into());
        self
    }

    pub fn add_tag(mut self, tag: impl Into<String>) -> Self {
        self.add_tags.push(tag.into());
        self
    }

    pub fn remove_tag(mut self, tag: impl Into<String>) -> Self {
        self.remove_tags.push(tag.into());
        self
    }

    /// Replace the [`writer::WriteOptions`] used by `execute`. See its
    /// docs for what each field controls. Defaults to fast (no fsync).
    pub fn write_options(mut self, opts: writer::WriteOptions) -> Self {
        self.write_options = opts;
        self
    }

    /// Convenience: enable durable writes (fsync data + parent dir).
    /// Equivalent to `.write_options(WriteOptions::durable())`.
    pub fn fsync(mut self, yes: bool) -> Self {
        self.write_options.fsync = yes;
        self
    }

    /// Compute the report without writing.
    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let (report, _writes) = self.compute(vault)?;
        Ok(report)
    }

    /// Plan, then apply each computed `WriteResult` to disk.
    ///
    /// Holds the vault-scoped exclusive lock (see [`crate::lock`]) for the
    /// entire duration of compute + writes, so concurrent mutations from
    /// other vaultdb-core consumers serialize cleanly. Each individual
    /// file write is atomic via tempfile+rename — readers never see a
    /// partial write.
    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        crate::lock::with_lock(&vault.root, || {
            let (report, writes) = self.compute(vault)?;
            for w in &writes {
                writer::apply_with(w, self.write_options).map_err(VaultdbError::Io)?;
            }
            Ok(report)
        })
    }

    fn compute(&self, vault: &Vault) -> Result<(MutationReport, Vec<WriteResult>)> {
        let folder_path = vault.resolve_folder(&self.folder)?;
        let load = vault.load_records_with_content(&folder_path, false, false)?;
        let needs_links = crate::filter::expr_uses_links(&self.filter);
        let link_index = if needs_links {
            Some(crate::links::LinkGraph::build_with_root(
                &load.records,
                Some(&vault.root),
            ))
        } else {
            None
        };

        let mut changes = Vec::new();
        let mut errors = Vec::new();
        let mut writes = Vec::new();

        for record in &load.records {
            if !crate::filter::evaluate_expr(&self.filter, record, &vault.root, link_index.as_ref())
            {
                continue;
            }

            // `load_records_with_content` (called above) guarantees raw_content
            // is populated. A None here means the loader's invariant was
            // violated — surface as Internal so the caller sees a real bug
            // rather than a per-record "soft" error.
            let mut content = record
                .raw_content
                .as_ref()
                .ok_or_else(|| {
                    VaultdbError::Internal(format!(
                        "record at {} has no raw_content; UpdateBuilder loaded without content",
                        record.path.display()
                    ))
                })?
                .clone();
            let original_content = content.clone();
            let mut wr_changes = Vec::new();
            let mut description_parts: Vec<String> = Vec::new();

            let result: Result<()> = (|| {
                for (field, value) in &self.set_fields {
                    let value_str = render_value_for_yaml(value);
                    let (new_content, change) = writer::set_field(&content, field, &value_str)?;
                    description_parts.push(format!("{}", change));
                    wr_changes.push(change);
                    content = new_content;
                }
                for field in &self.unset_fields {
                    let (new_content, change) = writer::unset_field(&content, field)?;
                    description_parts.push(format!("{}", change));
                    wr_changes.push(change);
                    content = new_content;
                }
                for tag in &self.add_tags {
                    let (new_content, change) = writer::add_tag(&content, tag)?;
                    description_parts.push(format!("{}", change));
                    wr_changes.push(change);
                    content = new_content;
                }
                for tag in &self.remove_tags {
                    let (new_content, change) = writer::remove_tag(&content, tag)?;
                    description_parts.push(format!("{}", change));
                    wr_changes.push(change);
                    content = new_content;
                }
                Ok(())
            })();

            match result {
                Ok(_) => {
                    if !wr_changes.is_empty() {
                        writes.push(WriteResult {
                            path: record.path.clone(),
                            original_content,
                            modified_content: content,
                            changes: wr_changes,
                        });
                        changes.push(PlannedChange {
                            path: record.path.clone(),
                            description: description_parts.join("; "),
                        });
                    }
                }
                Err(e) => errors.push(MutationError {
                    path: record.path.clone(),
                    message: e.to_string(),
                }),
            }
        }

        Ok((MutationReport { changes, errors }, writes))
    }
}

// ── DeleteBuilder ──────────────────────────────────────────────────────────

/// Build a delete mutation. Records matching `filter` are moved to
/// `<vault>/.trash/` by default (collision-safe). With `permanent(true)`,
/// files are removed entirely.
#[derive(Debug, Clone)]
pub struct DeleteBuilder {
    filter: Expr,
    folder: String,
    permanent: bool,
    write_options: writer::WriteOptions,
}

impl DeleteBuilder {
    pub fn new(folder: impl Into<String>, filter: Expr) -> Self {
        Self {
            filter,
            folder: folder.into(),
            permanent: false,
            write_options: writer::WriteOptions::default(),
        }
    }

    pub fn permanent(mut self, yes: bool) -> Self {
        self.permanent = yes;
        self
    }

    /// Replace the [`writer::WriteOptions`] used by `execute`.
    pub fn write_options(mut self, opts: writer::WriteOptions) -> Self {
        self.write_options = opts;
        self
    }

    /// Convenience: enable durable deletes (fsync the parent dir so the
    /// dirent change is on disk before this returns).
    pub fn fsync(mut self, yes: bool) -> Self {
        self.write_options.fsync = yes;
        self
    }

    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let folder_path = vault.resolve_folder(&self.folder)?;
        let load = vault.load_records(&folder_path, false, false)?;
        let needs_links = crate::filter::expr_uses_links(&self.filter);
        let link_index = if needs_links {
            Some(crate::links::LinkGraph::build_with_root(
                &load.records,
                Some(&vault.root),
            ))
        } else {
            None
        };

        let mut changes = Vec::new();
        for r in &load.records {
            if !crate::filter::evaluate_expr(&self.filter, r, &vault.root, link_index.as_ref()) {
                continue;
            }
            changes.push(PlannedChange {
                path: r.path.clone(),
                description: if self.permanent {
                    "delete (permanent)".to_string()
                } else {
                    "move to .trash/".to_string()
                },
            });
        }
        Ok(MutationReport {
            changes,
            errors: Vec::new(),
        })
    }

    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        crate::lock::with_lock(&vault.root, || {
            let report = self.plan(vault)?;
            let mut errors = Vec::new();
            // Track parents to fsync at the end if durability requested.
            let mut dirs_to_fsync: std::collections::BTreeSet<PathBuf> =
                std::collections::BTreeSet::new();

            if self.permanent {
                for change in &report.changes {
                    if let Err(e) = std::fs::remove_file(&change.path) {
                        errors.push(MutationError {
                            path: change.path.clone(),
                            message: format!("remove failed: {}", e),
                        });
                    } else if let Some(parent) = change.path.parent() {
                        dirs_to_fsync.insert(parent.to_path_buf());
                    }
                }
            } else {
                let trash_dir = vault.root.join(".trash");
                if !report.changes.is_empty() {
                    std::fs::create_dir_all(&trash_dir).map_err(VaultdbError::Io)?;
                }
                for change in &report.changes {
                    let dest = unique_in_dir(&trash_dir, &change.path);
                    if let Err(e) = std::fs::rename(&change.path, &dest) {
                        errors.push(MutationError {
                            path: change.path.clone(),
                            message: format!("trash failed: {}", e),
                        });
                    } else {
                        if let Some(parent) = change.path.parent() {
                            dirs_to_fsync.insert(parent.to_path_buf());
                        }
                        dirs_to_fsync.insert(trash_dir.clone());
                    }
                }
            }

            // Honour the fsync write option: flush every modified parent
            // directory's dirent so the metadata changes are durable.
            if self.write_options.fsync {
                for d in &dirs_to_fsync {
                    if let Err(e) = writer::fsync_dir(d) {
                        errors.push(MutationError {
                            path: d.clone(),
                            message: format!("fsync_dir failed: {}", e),
                        });
                    }
                }
            }

            Ok(MutationReport {
                changes: report.changes,
                errors,
            })
        })
    }
}

fn unique_in_dir(dir: &std::path::Path, src: &std::path::Path) -> PathBuf {
    let filename = src.file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let candidate = dir.join(filename);
    if !candidate.exists() {
        return candidate;
    }
    let stem = src.file_stem().and_then(|n| n.to_str()).unwrap_or("file");
    let ext = src.extension().and_then(|n| n.to_str()).unwrap_or("md");
    let mut i = 1;
    loop {
        let c = dir.join(format!("{}-{}.{}", stem, i, ext));
        if !c.exists() {
            return c;
        }
        i += 1;
    }
}

// ── MoveBuilder ────────────────────────────────────────────────────────────

/// Build a move mutation. Records matching `filter` are relocated into
/// `to_folder` (created if needed). Filename collisions at the destination
/// surface as `MutationError`s in the report.
#[derive(Debug, Clone)]
pub struct MoveBuilder {
    filter: Expr,
    folder: String,
    to_folder: String,
    write_options: writer::WriteOptions,
}

impl MoveBuilder {
    pub fn new(folder: impl Into<String>, to_folder: impl Into<String>, filter: Expr) -> Self {
        Self {
            filter,
            folder: folder.into(),
            to_folder: to_folder.into(),
            write_options: writer::WriteOptions::default(),
        }
    }

    /// Replace the [`writer::WriteOptions`] used by `execute`.
    pub fn write_options(mut self, opts: writer::WriteOptions) -> Self {
        self.write_options = opts;
        self
    }

    /// Convenience: enable durable moves (fsync source + dest dirents).
    pub fn fsync(mut self, yes: bool) -> Self {
        self.write_options.fsync = yes;
        self
    }

    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let folder_path = vault.resolve_folder(&self.folder)?;
        let to_path = vault.root.join(&self.to_folder);
        let load = vault.load_records(&folder_path, false, false)?;
        let needs_links = crate::filter::expr_uses_links(&self.filter);
        let link_index = if needs_links {
            Some(crate::links::LinkGraph::build_with_root(
                &load.records,
                Some(&vault.root),
            ))
        } else {
            None
        };

        let mut changes = Vec::new();
        let mut errors = Vec::new();

        for r in &load.records {
            if !crate::filter::evaluate_expr(&self.filter, r, &vault.root, link_index.as_ref()) {
                continue;
            }
            let filename = match r.path.file_name() {
                Some(n) => n,
                None => continue,
            };
            let dest = to_path.join(filename);
            if dest.exists() {
                errors.push(MutationError {
                    path: r.path.clone(),
                    message: format!(
                        "move conflict: {} already exists in {}",
                        filename.to_string_lossy(),
                        self.to_folder
                    ),
                });
                continue;
            }
            changes.push(PlannedChange {
                path: r.path.clone(),
                description: format!("move to {}", dest.display()),
            });
        }
        Ok(MutationReport { changes, errors })
    }

    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        crate::lock::with_lock(&vault.root, || {
            let to_path = vault.root.join(&self.to_folder);
            let report = self.plan(vault)?;
            if !report.changes.is_empty() {
                std::fs::create_dir_all(&to_path).map_err(VaultdbError::Io)?;
            }
            let mut errors = report.errors;
            let mut dirs_to_fsync: std::collections::BTreeSet<PathBuf> =
                std::collections::BTreeSet::new();
            for change in &report.changes {
                let filename = match change.path.file_name() {
                    Some(n) => n,
                    None => continue,
                };
                let dest = to_path.join(filename);
                if let Err(e) = std::fs::rename(&change.path, &dest) {
                    errors.push(MutationError {
                        path: change.path.clone(),
                        message: format!("rename failed: {}", e),
                    });
                } else {
                    if let Some(parent) = change.path.parent() {
                        dirs_to_fsync.insert(parent.to_path_buf());
                    }
                    dirs_to_fsync.insert(to_path.clone());
                }
            }

            if self.write_options.fsync {
                for d in &dirs_to_fsync {
                    if let Err(e) = writer::fsync_dir(d) {
                        errors.push(MutationError {
                            path: d.clone(),
                            message: format!("fsync_dir failed: {}", e),
                        });
                    }
                }
            }

            Ok(MutationReport {
                changes: report.changes,
                errors,
            })
        })
    }
}

// ── RenameBuilder ──────────────────────────────────────────────────────────

/// Build a rename mutation. The single record at `<folder>/<from>.md` is
/// renamed to `<folder>/<to>.md`, and every `[[wikilink]]` across the vault
/// pointing at `from` is rewritten to point at `to`.
///
/// Handled wikilink shapes: `[[from]]`, `[[from|alias]]`, `[[from#section]]`,
/// `[[from#section|alias]]`.
#[derive(Debug, Clone)]
pub struct RenameBuilder {
    folder: String,
    from: String,
    to: String,
    write_options: writer::WriteOptions,
}

impl RenameBuilder {
    pub fn new(folder: impl Into<String>, from: impl Into<String>, to: impl Into<String>) -> Self {
        Self {
            folder: folder.into(),
            from: from.into(),
            to: to.into(),
            write_options: writer::WriteOptions::default(),
        }
    }

    /// Replace the [`writer::WriteOptions`] used by `execute`.
    pub fn write_options(mut self, opts: writer::WriteOptions) -> Self {
        self.write_options = opts;
        self
    }

    /// Convenience: enable durable rename + backlink rewrites (fsync
    /// every modified file's data and every modified directory's dirent).
    pub fn fsync(mut self, yes: bool) -> Self {
        self.write_options.fsync = yes;
        self
    }

    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let folder_path = vault.resolve_folder(&self.folder)?;
        let source = folder_path.join(format!("{}.md", self.from));
        let dest = folder_path.join(format!("{}.md", self.to));

        let mut changes = Vec::new();
        let mut errors = Vec::new();

        if !source.is_file() {
            errors.push(MutationError {
                path: source.clone(),
                message: format!("source `{}` not found", self.from),
            });
            return Ok(MutationReport { changes, errors });
        }
        if dest.exists() {
            errors.push(MutationError {
                path: dest.clone(),
                message: format!("target `{}.md` already exists", self.to),
            });
            return Ok(MutationReport { changes, errors });
        }

        changes.push(PlannedChange {
            path: source.clone(),
            description: format!("rename to {}", dest.display()),
        });

        // Find every record that links to `self.from` and add a planned
        // backlink-rewrite change.
        let all = vault.load_records_with_content(&vault.root, true, false)?;
        let graph = crate::links::LinkGraph::build_with_root(&all.records, Some(&vault.root));
        for source_name in graph.incoming_links(&self.from) {
            if let Some(record) = graph.record_by_name(source_name) {
                changes.push(PlannedChange {
                    path: record.path.clone(),
                    description: format!("rewrite [[{}]] -> [[{}]]", self.from, self.to),
                });
            }
        }

        Ok(MutationReport { changes, errors })
    }

    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        crate::lock::with_lock(&vault.root, || {
            // Recover any pending journals from previous crashed renames
            // before we start a new one. Without this, a stale journal
            // could be replayed *after* our new rename, producing
            // surprising results (e.g. backlink rewrites for an old
            // rename happening on top of a new one). Replay first means
            // the vault is in a clean known state when we start.
            crate::journal::replay_all(&vault.root)?;

            let folder_path = vault.resolve_folder(&self.folder)?;
            let source = folder_path.join(format!("{}.md", self.from));
            let dest = folder_path.join(format!("{}.md", self.to));

            let report = self.plan(vault)?;
            // If the plan reported errors at the source/dest stage, don't proceed.
            if !report.errors.is_empty() {
                return Ok(report);
            }

            // Build and write the journal BEFORE any disk-modifying
            // step. If the process dies between this point and the
            // final delete-journal call, the next mutation (or an
            // explicit `Vault::recover` call) will replay it.
            let backlinks: Vec<PathBuf> = report
                .changes
                .iter()
                .skip(1) // first change is the rename itself
                .map(|c| c.path.clone())
                .collect();
            let journal = crate::journal::RenameJournal {
                source: source.clone(),
                dest: dest.clone(),
                from_name: self.from.clone(),
                to_name: self.to.clone(),
                backlinks,
            };
            let journal_path = crate::journal::write(&vault.root, &journal)?;

            // Now do the rename. If this fails, drop the journal — the
            // vault is unchanged, no recovery work to do.
            if let Err(e) = std::fs::rename(&source, &dest) {
                crate::journal::delete(&journal_path);
                return Ok(MutationReport {
                    changes: report.changes,
                    errors: vec![MutationError {
                        path: source,
                        message: format!("rename failed: {}", e),
                    }],
                });
            }

            // Rewrite incoming wikilinks atomically (tempfile + rename
            // per file). Each rewrite is itself idempotent so a partial
            // run + journal replay reaches the same end state.
            let mut errors = Vec::new();
            let mut dirs_to_fsync: std::collections::BTreeSet<PathBuf> =
                std::collections::BTreeSet::new();
            if let Some(parent) = source.parent() {
                dirs_to_fsync.insert(parent.to_path_buf());
            }
            if let Some(parent) = dest.parent() {
                dirs_to_fsync.insert(parent.to_path_buf());
            }
            for change in report.changes.iter().skip(1) {
                let path = &change.path;
                let content = match std::fs::read_to_string(path) {
                    Ok(c) => c,
                    Err(e) => {
                        errors.push(MutationError {
                            path: path.clone(),
                            message: format!("read failed: {}", e),
                        });
                        continue;
                    }
                };
                let new_content = rewrite_wikilinks(&content, &self.from, &self.to);
                if new_content == content {
                    continue;
                }
                if let Err(e) = writer::atomic_write_with(path, &new_content, self.write_options) {
                    errors.push(MutationError {
                        path: path.clone(),
                        message: format!("write failed: {}", e),
                    });
                }
            }

            // Fsync every dir whose dirent changed (the source's parent
            // for the removed entry, the dest's parent for the added
            // entry). atomic_write_with already fsynced the backlinks'
            // parents internally when fsync was on.
            if self.write_options.fsync {
                for d in &dirs_to_fsync {
                    if let Err(e) = writer::fsync_dir(d) {
                        errors.push(MutationError {
                            path: d.clone(),
                            message: format!("fsync_dir failed: {}", e),
                        });
                    }
                }
            }

            // All rewrites attempted. If any failed, leave the journal
            // so the next replay sweep retries them. If everything
            // succeeded, drop the journal — recovery has nothing to do.
            if errors.is_empty() {
                crate::journal::delete(&journal_path);
            }

            Ok(MutationReport {
                changes: report.changes,
                errors,
            })
        })
    }
}

// ── CreateBuilder ──────────────────────────────────────────────────────────

/// Build a create mutation. Writes a new `.md` file under `folder` with
/// name `name` (the `.md` extension is appended automatically).
///
/// Frontmatter is composed in three layers, in order of precedence:
///
/// 1. Template — if `template(path)` is set, the template's frontmatter
///    is parsed as the base. Template body is preserved.
/// 2. `--set` overrides — anything set via `set()` overrides matching
///    template fields.
/// 3. Schema defaults — when `with_schema(schema)` is supplied, every
///    `default:` / `default_expr:` whose field isn't already set by
///    the template or `--set` is applied.
///
/// After composition, schema's `required:` list is checked. Missing
/// required fields surface as `MutationError`s in the report, with the
/// file NOT written.
#[derive(Debug, Clone)]
pub struct CreateBuilder {
    folder: String,
    name: String,
    template: Option<String>,
    set_fields: Vec<(String, Value)>,
    schema: Option<crate::schema::CollectionSchema>,
    write_options: writer::WriteOptions,
}

impl CreateBuilder {
    pub fn new(folder: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            folder: folder.into(),
            name: name.into(),
            template: None,
            set_fields: Vec::new(),
            schema: None,
            write_options: writer::WriteOptions::default(),
        }
    }

    /// Path to a template file, relative to the vault root. The
    /// template's frontmatter forms the base of the new note; its body
    /// is preserved.
    pub fn template(mut self, path: impl Into<String>) -> Self {
        self.template = Some(path.into());
        self
    }

    /// Set (or override) a frontmatter field. Layered on top of the
    /// template; layered under schema defaults.
    pub fn set(mut self, field: impl Into<String>, value: Value) -> Self {
        self.set_fields.push((field.into(), value));
        self
    }

    /// Attach the collection schema for this folder. When set:
    /// `default:` / `default_expr:` fields are applied for any field
    /// the user didn't supply, and `required:` is enforced before
    /// writing.
    pub fn with_schema(mut self, schema: crate::schema::CollectionSchema) -> Self {
        self.schema = Some(schema);
        self
    }

    pub fn write_options(mut self, opts: writer::WriteOptions) -> Self {
        self.write_options = opts;
        self
    }

    pub fn fsync(mut self, yes: bool) -> Self {
        self.write_options.fsync = yes;
        self
    }

    /// Compute the planned change without writing.
    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let (report, _) = self.compute(vault)?;
        Ok(report)
    }

    /// Plan and also return the file content that would be written.
    /// Used by the CLI's `--dry-run` for create — the post-defaults
    /// frontmatter is the value of the preview.
    pub fn plan_with_content(&self, vault: &Vault) -> Result<(MutationReport, Option<String>)> {
        let (report, write) = self.compute(vault)?;
        Ok((report, write.map(|w| w.modified_content)))
    }

    /// Plan, then write the file atomically. Holds the vault-scoped
    /// lock so concurrent creates against the same vault serialise
    /// cleanly.
    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        crate::lock::with_lock(&vault.root, || {
            let (report, write) = self.compute(vault)?;
            if !report.errors.is_empty() {
                return Ok(report);
            }
            if let Some(w) = write {
                if let Some(parent) = w.path.parent()
                    && !parent.exists()
                {
                    std::fs::create_dir_all(parent).map_err(VaultdbError::Io)?;
                }
                writer::atomic_write_with(&w.path, &w.modified_content, self.write_options)
                    .map_err(VaultdbError::Io)?;
            }
            Ok(report)
        })
    }

    fn compute(&self, vault: &Vault) -> Result<(MutationReport, Option<WriteResult>)> {
        // We DON'T require the folder to exist on disk — create can
        // make it. `vault.resolve_folder` errors on missing dirs, so
        // we build the path ourselves and let the writer create the
        // parent at execute time.
        let folder_path = vault.root.join(&self.folder);
        let filename = format!("{}.md", self.name);
        let dest = folder_path.join(&filename);

        let mut changes = Vec::new();
        let mut errors = Vec::new();

        if dest.exists() {
            errors.push(MutationError {
                path: dest.clone(),
                message: format!("file already exists: {}", dest.display()),
            });
            return Ok((MutationReport { changes, errors }, None));
        }

        // 1. Template — parse its frontmatter into a typed map, keep
        //    its body verbatim. No template → empty map + minimal body
        //    (`# {name}` so the note isn't blank).
        let (mut fields, body) = match &self.template {
            Some(tmpl) => {
                let tmpl_path = vault.root.join(tmpl);
                if !tmpl_path.is_file() {
                    errors.push(MutationError {
                        path: tmpl_path.clone(),
                        message: format!("template not found: {}", tmpl_path.display()),
                    });
                    return Ok((MutationReport { changes, errors }, None));
                }
                let raw = std::fs::read_to_string(&tmpl_path).map_err(VaultdbError::Io)?;
                split_template(&raw)
            }
            None => (
                std::collections::BTreeMap::<String, Value>::new(),
                format!("\n# {}\n", self.name),
            ),
        };

        // 2. --set overrides.
        for (k, v) in &self.set_fields {
            fields.insert(k.clone(), v.clone());
        }

        // 3. Schema defaults (only for unset fields).
        if let Some(schema) = &self.schema {
            for (name, fs) in &schema.fields {
                if fields.contains_key(name) {
                    continue;
                }
                if let Some(default) = &fs.default {
                    fields.insert(name.clone(), default.clone());
                } else if let Some(expr) = &fs.default_expr {
                    match crate::schema::resolve_default_expr(expr) {
                        Ok(v) => {
                            fields.insert(name.clone(), v);
                        }
                        Err(e) => {
                            errors.push(MutationError {
                                path: dest.clone(),
                                message: format!("resolving default_expr for '{}': {}", name, e),
                            });
                        }
                    }
                }
            }

            // 4. Required-field check, AFTER defaults have been applied.
            for req in &schema.required {
                let satisfied = matches!(fields.get(req), Some(v) if !matches!(v, Value::Null));
                if !satisfied {
                    errors.push(MutationError {
                        path: dest.clone(),
                        message: format!("required field missing: '{}'", req),
                    });
                }
            }
        }

        if !errors.is_empty() {
            return Ok((MutationReport { changes, errors }, None));
        }

        // 5. Render frontmatter + body. serde_yaml emits an unprefixed
        //    mapping; we wrap with `---` delimiters. Empty field map
        //    still gets `---\n---\n` so consumers can find a parse anchor.
        let frontmatter_yaml = if fields.is_empty() {
            String::new()
        } else {
            serde_yaml::to_string(&fields)
                .map_err(|e| VaultdbError::SchemaError(format!("rendering frontmatter: {}", e)))?
        };
        let content = if frontmatter_yaml.is_empty() {
            format!("---\n---\n{}", body)
        } else {
            format!("---\n{}---\n{}", frontmatter_yaml, body)
        };

        let field_count = fields.len();
        let field_summary: String = fields.keys().cloned().collect::<Vec<_>>().join(", ");
        let description = if field_count == 0 {
            "create (no frontmatter fields)".to_string()
        } else {
            format!("create with {} field(s): {}", field_count, field_summary)
        };

        changes.push(PlannedChange {
            path: dest.clone(),
            description,
        });

        let write = WriteResult {
            path: dest,
            original_content: String::new(),
            modified_content: content,
            changes: Vec::new(),
        };

        Ok((MutationReport { changes, errors }, Some(write)))
    }
}

/// Parse a template's frontmatter + body into a typed field map and
/// the verbatim body string. Returns an empty map + the whole raw
/// content as body when the template has no frontmatter delimiters.
fn split_template(raw: &str) -> (std::collections::BTreeMap<String, Value>, String) {
    use crate::frontmatter::{extract_frontmatter, parse_frontmatter};
    match extract_frontmatter(raw) {
        Some((yaml_text, body_start)) => {
            let fields = parse_frontmatter(yaml_text).unwrap_or_default();
            let body = raw[body_start..].to_string();
            (fields, body)
        }
        None => (std::collections::BTreeMap::new(), raw.to_string()),
    }
}

/// Rewrite `[[from]]` (and `[[from|alias]]`, `[[from#section]]`,
/// `[[from#section|alias]]`) to point at `to`.
pub(crate) fn rewrite_wikilinks(content: &str, from: &str, to: &str) -> String {
    content
        .replace(&format!("[[{}]]", from), &format!("[[{}]]", to))
        .replace(&format!("[[{}|", from), &format!("[[{}|", to))
        .replace(&format!("[[{}#", from), &format!("[[{}#", to))
}

/// Render a `Value` as a YAML scalar suitable for inline frontmatter.
///
/// String values are quoted via `writer::quote_value` to match the existing
/// writer's escape rules. Lists and maps fall back to `serde_yaml::to_string`
/// (trimmed) for a flow-style representation.
fn render_value_for_yaml(v: &Value) -> String {
    match v {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Integer(i) => i.to_string(),
        Value::Float(f) => f.to_string(),
        Value::String(s) => writer::quote_value(s),
        Value::List(_) | Value::Map(_) => {
            let yaml = serde_yaml::to_string(v).unwrap_or_default();
            yaml.trim_end().to_string()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::query::Predicate;

    #[test]
    fn update_builder_chains() {
        let filter = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("active".into()),
        });
        let b = UpdateBuilder::new("notes", filter)
            .set("priority", Value::Integer(1))
            .unset("draft")
            .add_tag("urgent")
            .remove_tag("stale");
        assert_eq!(b.set_fields.len(), 1);
        assert_eq!(b.unset_fields.len(), 1);
        assert_eq!(b.add_tags.len(), 1);
        assert_eq!(b.remove_tags.len(), 1);
    }

    #[test]
    fn delete_builder_trash_moves_to_dot_trash() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/a.md"), "---\nstatus: stale\n---\n").unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let filter = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("stale".into()),
        });
        let builder = DeleteBuilder::new("notes", filter);
        let report = builder.execute(&vault).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.errors.len(), 0);
        assert!(!dir.path().join("notes/a.md").exists());
        assert!(dir.path().join(".trash/a.md").exists());
    }

    #[test]
    fn delete_builder_permanent_removes_file() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/a.md"), "---\nstatus: stale\n---\n").unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let filter = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("stale".into()),
        });
        let builder = DeleteBuilder::new("notes", filter).permanent(true);
        builder.execute(&vault).unwrap();
        assert!(!dir.path().join("notes/a.md").exists());
        assert!(!dir.path().join(".trash/a.md").exists());
    }

    #[test]
    fn move_builder_relocates_files() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/a.md"),
            "---\nstatus: archived\n---\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let filter = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("archived".into()),
        });
        let builder = MoveBuilder::new("notes", "archive", filter);
        let report = builder.execute(&vault).unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.errors.len(), 0);
        assert!(!dir.path().join("notes/a.md").exists());
        assert!(dir.path().join("archive/a.md").exists());
    }

    #[test]
    fn rename_builder_renames_and_rewrites_links() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/old.md"),
            "---\nstatus: x\n---\nBody\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/source.md"),
            "---\nstatus: y\n---\nLinks to [[old]] and [[old|alias]] and [[old#section]].\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());

        let builder = RenameBuilder::new("notes", "old", "new");
        let report = builder.execute(&vault).unwrap();
        // 1 rename + 1 backlink rewrite = 2 changes
        assert_eq!(report.changes.len(), 2);
        assert_eq!(report.errors.len(), 0);
        assert!(!dir.path().join("notes/old.md").exists());
        assert!(dir.path().join("notes/new.md").exists());
        let source_after = fs::read_to_string(dir.path().join("notes/source.md")).unwrap();
        assert!(source_after.contains("[[new]]"));
        assert!(source_after.contains("[[new|alias]]"));
        assert!(source_after.contains("[[new#section]]"));
        assert!(!source_after.contains("[[old"));
    }

    #[test]
    fn rename_builder_target_conflict_returns_error() {
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(dir.path().join("notes/old.md"), "---\nstatus: x\n---\n").unwrap();
        fs::write(dir.path().join("notes/new.md"), "---\nstatus: y\n---\n").unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let report = RenameBuilder::new("notes", "old", "new")
            .execute(&vault)
            .unwrap();
        assert_eq!(report.changes.len(), 0);
        assert_eq!(report.errors.len(), 1);
        // Source file should be untouched
        assert!(dir.path().join("notes/old.md").exists());
    }

    #[test]
    fn update_builder_plan_and_execute_against_a_temp_vault() {
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/a.md"),
            "---\nstatus: active\n---\nBody A\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/b.md"),
            "---\nstatus: pending\n---\nBody B\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());

        let filter = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("active".into()),
        });
        let builder = UpdateBuilder::new("notes", filter).set("priority", Value::Integer(1));

        // plan() does not touch disk
        let plan_report = builder.plan(&vault).unwrap();
        assert_eq!(plan_report.changes.len(), 1);
        assert_eq!(plan_report.errors.len(), 0);
        assert!(plan_report.changes[0].path.ends_with("a.md"));
        let before = fs::read_to_string(dir.path().join("notes/a.md")).unwrap();
        assert!(!before.contains("priority"));

        // execute() applies the change
        let exec_report = builder.execute(&vault).unwrap();
        assert_eq!(exec_report.changes.len(), 1);
        let after = fs::read_to_string(dir.path().join("notes/a.md")).unwrap();
        assert!(after.contains("priority"));
        // b.md was NOT touched (its status is pending, doesn't match the filter)
        let b_after = fs::read_to_string(dir.path().join("notes/b.md")).unwrap();
        assert!(!b_after.contains("priority"));
    }

    #[test]
    fn write_options_fsync_propagates_through_update_builder() {
        // We can't easily test that fsync was actually called (no kernel
        // hook from user space), but we can test that:
        // 1. The fsync(true) builder method round-trips through to
        //    write_options.fsync = true.
        // 2. An update with fsync(true) still produces correct content
        //    on disk.
        // 3. WriteOptions::durable() is a shorthand for { fsync: true }.
        use std::fs;
        use tempfile::TempDir;

        // 1. The fluent API.
        let f1 = Expr::Predicate(Predicate::Equals {
            field: "x".into(),
            value: Value::Integer(1),
        });
        let b = UpdateBuilder::new("notes", f1).fsync(true);
        assert!(b.write_options.fsync);

        let f2 = Expr::Predicate(Predicate::Equals {
            field: "x".into(),
            value: Value::Integer(1),
        });
        let b =
            UpdateBuilder::new("notes", f2).write_options(crate::writer::WriteOptions::durable());
        assert!(b.write_options.fsync);

        // 2. Real execute with fsync=true still produces correct content.
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/durable.md"),
            "---\nstatus: active\n---\nBody.\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());

        let f3 = Expr::Predicate(Predicate::Equals {
            field: "status".into(),
            value: Value::String("active".into()),
        });
        let report = UpdateBuilder::new("notes", f3)
            .set("priority", Value::Integer(99))
            .fsync(true)
            .execute(&vault)
            .unwrap();
        assert_eq!(report.changes.len(), 1);
        assert_eq!(report.errors.len(), 0);

        let after = fs::read_to_string(dir.path().join("notes/durable.md")).unwrap();
        assert!(after.contains("priority: 99"));
        assert!(after.contains("status: active"));
    }

    #[test]
    fn rename_clean_run_leaves_no_journal_behind() {
        // Successful rename writes a journal then deletes it. After the
        // call, the journal directory should be empty (or absent).
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/old.md"),
            "---\nstatus: x\n---\nBody\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/source.md"),
            "---\nstatus: y\n---\nLinks to [[old]].\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());

        RenameBuilder::new("notes", "old", "new")
            .execute(&vault)
            .unwrap();

        let pending = crate::journal::list_pending(dir.path()).unwrap();
        assert!(
            pending.is_empty(),
            "successful rename must not leave journals behind: {:?}",
            pending
        );
    }

    #[test]
    fn rename_recovers_from_pre_existing_journal() {
        // Simulate a crashed rename by hand-writing a journal that points
        // at an unrenamed source file with stale backlinks. Vault::recover
        // should pick it up and complete the work.
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        let source = dir.path().join("notes/Stanford.md");
        let dest = dir.path().join("notes/Stanford University.md");
        let backlink = dir.path().join("notes/Application.md");
        fs::write(&source, "---\nkind: university\n---\nMain note.\n").unwrap();
        fs::write(
            &backlink,
            "---\nkind: application\n---\nApplied to [[Stanford]].\n",
        )
        .unwrap();

        // Plant a journal as if a previous rename had crashed mid-flight.
        let journal = crate::journal::RenameJournal {
            source: source.clone(),
            dest: dest.clone(),
            from_name: "Stanford".into(),
            to_name: "Stanford University".into(),
            backlinks: vec![backlink.clone()],
        };
        crate::journal::write(dir.path(), &journal).unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let recovered = vault.recover().unwrap();
        assert_eq!(recovered, 1, "expected exactly one journal replayed");

        // Source renamed, backlink rewritten, journal gone.
        assert!(!source.exists());
        assert!(dest.is_file());
        let backlink_content = fs::read_to_string(&backlink).unwrap();
        assert!(backlink_content.contains("[[Stanford University]]"));
        assert!(!backlink_content.contains("[[Stanford]]"));
        assert!(crate::journal::list_pending(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn rename_replays_pending_journal_before_starting_new_rename() {
        // A pending journal from a previous crashed rename must be
        // replayed before the next mutation starts. Otherwise we'd be
        // operating on a stale view of the vault.
        use std::fs;
        use tempfile::TempDir;
        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();

        // Pre-existing files: A.md (will be renamed to B.md by the
        // pending journal), and C.md (will be renamed to D.md by the
        // new RenameBuilder call).
        let a = dir.path().join("notes/A.md");
        let b = dir.path().join("notes/B.md");
        let c = dir.path().join("notes/C.md");
        let d = dir.path().join("notes/D.md");
        fs::write(&a, "---\n---\nA body.\n").unwrap();
        fs::write(&c, "---\n---\nC body.\n").unwrap();

        // Plant a pending journal that says "rename A -> B".
        crate::journal::write(
            dir.path(),
            &crate::journal::RenameJournal {
                source: a.clone(),
                dest: b.clone(),
                from_name: "A".into(),
                to_name: "B".into(),
                backlinks: vec![],
            },
        )
        .unwrap();

        // Now call execute() on a separate rename C -> D.
        let vault = Vault::with_root(dir.path().to_path_buf());
        RenameBuilder::new("notes", "C", "D")
            .execute(&vault)
            .unwrap();

        // BOTH renames must have completed: the journal-replay before the
        // new rename did A -> B, and the new RenameBuilder did C -> D.
        assert!(!a.exists(), "A.md should be gone (replayed journal)");
        assert!(b.is_file(), "B.md should exist (replayed journal)");
        assert!(!c.exists(), "C.md should be gone (new rename)");
        assert!(d.is_file(), "D.md should exist (new rename)");

        // No leftover journals.
        assert!(crate::journal::list_pending(dir.path()).unwrap().is_empty());
    }

    #[test]
    fn concurrent_updates_serialize_via_vault_lock() {
        // Two threads each run an UpdateBuilder against the same vault.
        // Without the lock, both would read a.md, both would compute their
        // edit against the same baseline, and the second writer would
        // clobber the first's change. With the lock, the second thread
        // waits for the first to finish, re-reads the (now-updated) file,
        // and applies its change on top. The result should reflect both
        // edits, in some serial order.
        use std::fs;
        use std::sync::Arc;
        use std::thread;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/race.md"),
            "---\nstatus: active\n---\nBody.\n",
        )
        .unwrap();

        let vault_path = Arc::new(dir.path().to_path_buf());

        let p1 = Arc::clone(&vault_path);
        let t1 = thread::spawn(move || {
            let vault = Vault::with_root((*p1).clone());
            let filter = Expr::Predicate(Predicate::Equals {
                field: "status".into(),
                value: Value::String("active".into()),
            });
            UpdateBuilder::new("notes", filter)
                .set("touched_by_t1", Value::Integer(1))
                .execute(&vault)
                .expect("t1 execute")
        });

        let p2 = Arc::clone(&vault_path);
        let t2 = thread::spawn(move || {
            let vault = Vault::with_root((*p2).clone());
            let filter = Expr::Predicate(Predicate::Equals {
                field: "status".into(),
                value: Value::String("active".into()),
            });
            UpdateBuilder::new("notes", filter)
                .set("touched_by_t2", Value::Integer(1))
                .execute(&vault)
                .expect("t2 execute")
        });

        let r1 = t1.join().unwrap();
        let r2 = t2.join().unwrap();
        assert_eq!(r1.errors.len(), 0);
        assert_eq!(r2.errors.len(), 0);

        // The final file content must contain BOTH edits. If the lock
        // failed, exactly one would survive (whichever thread wrote last)
        // because the slower thread's snapshot would be stale.
        let final_content = fs::read_to_string(dir.path().join("notes/race.md")).unwrap();
        assert!(
            final_content.contains("touched_by_t1"),
            "t1's edit lost; concurrent writer race: {}",
            final_content
        );
        assert!(
            final_content.contains("touched_by_t2"),
            "t2's edit lost; concurrent writer race: {}",
            final_content
        );
    }

    #[test]
    fn atomic_write_does_not_leave_partial_files_on_failed_writes() {
        // Direct test of the writer::atomic_write contract: if the write
        // is interrupted, readers should never observe partial content.
        // We simulate a failed write by trying to atomic_write to a path
        // whose parent doesn't exist — that fails cleanly, and the
        // ORIGINAL file (if any) should be untouched.
        use std::fs;
        use tempfile::TempDir;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("subdir/that-does-not-exist/x.md");
        // Pre-existing target shouldn't be touched (there is none here, but
        // the atomic_write itself must fail cleanly without side effects).
        let result = crate::writer::atomic_write(&target, "new content");
        assert!(
            result.is_err(),
            "expected atomic_write to fail when parent dir doesn't exist"
        );

        // Now: create a target with original content, then try a write
        // that succeeds. Verify the file is fully replaced (no merge).
        let real_dir = dir.path().join("real");
        fs::create_dir(&real_dir).unwrap();
        let real_target = real_dir.join("x.md");
        fs::write(&real_target, "original").unwrap();
        crate::writer::atomic_write(&real_target, "replacement").unwrap();
        let after = fs::read_to_string(&real_target).unwrap();
        assert_eq!(after, "replacement");

        // No leftover tempfile in the directory.
        let leftovers: Vec<_> = fs::read_dir(&real_dir)
            .unwrap()
            .flatten()
            .filter(|e| e.file_name().to_string_lossy().starts_with(".tmp"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "expected no tempfile leftovers, found: {:?}",
            leftovers.iter().map(|e| e.path()).collect::<Vec<_>>()
        );
    }

    // ── Phase 3: CreateBuilder ────────────────────────────────────────

    use crate::schema::{CollectionSchema, FieldSchema};

    fn movie_schema() -> CollectionSchema {
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "db-table".into(),
            FieldSchema {
                field_type: "string".into(),
                enum_values: vec![Value::String("movie".into())],
                min: None,
                max: None,
                default: Some(Value::String("movie".into())),
                default_expr: None,
            },
        );
        fields.insert(
            "status".into(),
            FieldSchema {
                field_type: "string".into(),
                enum_values: vec![
                    Value::String("to-watch".into()),
                    Value::String("watched".into()),
                ],
                min: None,
                max: None,
                default: Some(Value::String("to-watch".into())),
                default_expr: None,
            },
        );
        fields.insert(
            "director".into(),
            FieldSchema {
                field_type: "string".into(),
                enum_values: vec![],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );
        fields.insert(
            "year".into(),
            FieldSchema {
                field_type: "integer".into(),
                enum_values: vec![],
                min: None,
                max: None,
                default: None,
                default_expr: None,
            },
        );
        CollectionSchema {
            description: None,
            folder: "Notes/movie".into(),
            filter: vec![],
            required: vec![
                "db-table".into(),
                "director".into(),
                "status".into(),
                "year".into(),
            ],
            fields,
        }
    }

    fn vault_with_obsidian() -> tempfile::TempDir {
        let dir = tempfile::TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".obsidian")).unwrap();
        dir
    }

    #[test]
    fn create_without_schema_writes_minimal_file() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let report = CreateBuilder::new("Notes/movie", "Dune")
            .execute(&vault)
            .unwrap();
        assert_eq!(report.errors.len(), 0);
        assert_eq!(report.changes.len(), 1);
        let written = dir.path().join("Notes/movie/Dune.md");
        assert!(written.is_file());
        let content = std::fs::read_to_string(&written).unwrap();
        // Empty frontmatter + "# Dune" body.
        assert!(content.contains("---\n---"));
        assert!(content.contains("# Dune"));
    }

    #[test]
    fn create_with_set_writes_typed_frontmatter() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        CreateBuilder::new("Notes/movie", "Dune")
            .set("director", Value::String("Denis Villeneuve".into()))
            .set("year", Value::Integer(2021))
            .execute(&vault)
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
        assert!(content.contains("director: Denis Villeneuve"));
        // YAML int rendering — no quotes.
        assert!(content.contains("year: 2021"));
    }

    #[test]
    fn create_fills_schema_defaults() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        CreateBuilder::new("Notes/movie", "Dune")
            .with_schema(movie_schema())
            .set("director", Value::String("Denis Villeneuve".into()))
            .set("year", Value::Integer(2021))
            .execute(&vault)
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
        // Defaults applied.
        assert!(content.contains("db-table: movie"));
        assert!(content.contains("status: to-watch"));
        // User-supplied values preserved.
        assert!(content.contains("director: Denis Villeneuve"));
        assert!(content.contains("year: 2021"));
    }

    #[test]
    fn create_set_overrides_default() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        CreateBuilder::new("Notes/movie", "Watched")
            .with_schema(movie_schema())
            .set("director", Value::String("X".into()))
            .set("year", Value::Integer(2020))
            .set("status", Value::String("watched".into()))
            .execute(&vault)
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("Notes/movie/Watched.md")).unwrap();
        assert!(content.contains("status: watched"));
        assert!(!content.contains("status: to-watch"));
    }

    #[test]
    fn create_rejects_missing_required_before_writing() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        // No director / year → required check should fail.
        let report = CreateBuilder::new("Notes/movie", "Blank")
            .with_schema(movie_schema())
            .execute(&vault)
            .unwrap();
        assert!(!report.errors.is_empty());
        assert!(report.errors.iter().any(|e| e.message.contains("director")));
        assert!(report.errors.iter().any(|e| e.message.contains("year")));
        // File must NOT have been written.
        assert!(!dir.path().join("Notes/movie/Blank.md").exists());
    }

    #[test]
    fn create_rejects_existing_file() {
        let dir = vault_with_obsidian();
        std::fs::create_dir_all(dir.path().join("Notes/movie")).unwrap();
        std::fs::write(dir.path().join("Notes/movie/Dune.md"), "existing\n").unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let report = CreateBuilder::new("Notes/movie", "Dune")
            .execute(&vault)
            .unwrap();
        assert_eq!(report.errors.len(), 1);
        assert!(report.errors[0].message.contains("already exists"));
        // Original file preserved.
        let content = std::fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
        assert_eq!(content, "existing\n");
    }

    #[test]
    fn create_resolves_default_expr_today() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let mut fields = std::collections::BTreeMap::new();
        fields.insert(
            "due".into(),
            FieldSchema {
                field_type: "date".into(),
                enum_values: vec![],
                min: None,
                max: None,
                default: None,
                default_expr: Some("today".into()),
            },
        );
        let schema = CollectionSchema {
            description: None,
            folder: "tasks".into(),
            filter: vec![],
            required: vec![],
            fields,
        };
        CreateBuilder::new("tasks", "t1")
            .with_schema(schema)
            .execute(&vault)
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("tasks/t1.md")).unwrap();
        // Match shape, not exact value (date depends on when test runs).
        let today = crate::record::today_string();
        assert!(
            content.contains(&format!("due: {}", today)),
            "expected due={} in: {}",
            today,
            content
        );
    }

    #[test]
    fn create_plan_does_not_touch_disk() {
        let dir = vault_with_obsidian();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let (report, content) = CreateBuilder::new("Notes/movie", "Dune")
            .with_schema(movie_schema())
            .set("director", Value::String("DV".into()))
            .set("year", Value::Integer(2021))
            .plan_with_content(&vault)
            .unwrap();
        assert_eq!(report.errors.len(), 0);
        assert_eq!(report.changes.len(), 1);
        assert!(!dir.path().join("Notes/movie/Dune.md").exists());
        let c = content.unwrap();
        assert!(c.contains("director: DV"));
        assert!(c.contains("db-table: movie")); // default applied even in plan
    }

    #[test]
    fn create_from_template_preserves_body_and_merges_frontmatter() {
        let dir = vault_with_obsidian();
        std::fs::create_dir_all(dir.path().join("templates")).unwrap();
        std::fs::write(
            dir.path().join("templates/movie.md"),
            "---\nstatus: to-watch\naliases: []\n---\n\n# Title\n\nReview goes here.\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        CreateBuilder::new("Notes/movie", "Dune")
            .template("templates/movie.md")
            .set("year", Value::Integer(2021))
            .execute(&vault)
            .unwrap();
        let content = std::fs::read_to_string(dir.path().join("Notes/movie/Dune.md")).unwrap();
        // Template field preserved.
        assert!(content.contains("status: to-watch"));
        // --set added.
        assert!(content.contains("year: 2021"));
        // Body preserved.
        assert!(content.contains("Review goes here"));
    }
}
