use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::error::{Result, VaultdbError};
use crate::frontmatter;
use crate::record::Record;

/// Records loaded from a folder, with per-file parse diagnostics.
///
/// Files with malformed YAML frontmatter appear in `parse_errors` rather than
/// being silently dropped. Files without frontmatter at all are loaded as
/// empty records (this is intentional — they remain queryable by virtual
/// fields like `_name` / `_path`).
#[derive(Debug, Clone)]
pub struct LoadResult {
    pub records: Vec<Record>,
    pub parse_errors: Vec<crate::error::ParseError>,
}

/// Represents a discovered Obsidian vault.
pub struct Vault {
    pub root: PathBuf,
}

impl Vault {
    /// Discover vault root by walking up from `start` looking for `.obsidian/`.
    pub fn discover(start: &Path) -> Result<Self> {
        let mut current = start.to_path_buf();
        loop {
            if current.join(".obsidian").is_dir() {
                return Ok(Vault { root: current });
            }
            if !current.pop() {
                return Err(VaultdbError::VaultNotFound(start.display().to_string()));
            }
        }
    }

    /// Create a Vault with an explicit root path (skips discovery).
    pub fn with_root(root: PathBuf) -> Self {
        Vault { root }
    }

    /// Resolve a folder argument (relative to vault root) to an absolute path.
    pub fn resolve_folder(&self, folder: &str) -> Result<PathBuf> {
        let path = self.root.join(folder);
        if path.is_dir() {
            Ok(path)
        } else {
            Err(VaultdbError::FolderNotFound(folder.to_string()))
        }
    }

    /// List all .md files in a folder. If `recursive`, walks subdirectories.
    pub fn list_files(&self, folder: &Path, recursive: bool) -> Result<Vec<PathBuf>> {
        let mut files = Vec::new();

        if recursive {
            for entry in WalkDir::new(folder)
                .follow_links(false)
                .into_iter()
                .filter_entry(|e| {
                    // Skip hidden directories
                    !e.file_name().to_str().is_some_and(|s| s.starts_with('.'))
                })
            {
                let entry = entry.map_err(|e| std::io::Error::other(e.to_string()))?;
                if entry.file_type().is_file()
                    && entry.path().extension().is_some_and(|ext| ext == "md")
                {
                    files.push(entry.into_path());
                }
            }
        } else {
            for entry in std::fs::read_dir(folder)? {
                let entry = entry?;
                let path = entry.path();
                if path.is_file() && path.extension().is_some_and(|ext| ext == "md") {
                    files.push(path);
                }
            }
        }

        files.sort();
        Ok(files)
    }

    /// Load records from a folder, collecting per-file parse diagnostics.
    ///
    /// Files with no frontmatter are loaded as empty records (queryable via
    /// virtual fields). Files with invalid frontmatter are collected into
    /// `LoadResult.parse_errors` rather than dropped.
    ///
    /// `verbose` is preserved for compatibility with the CLI's `-v` flag — it
    /// causes parse errors to also be logged to stderr as they're encountered.
    /// Library consumers that don't want stderr logging should pass `false` and
    /// inspect `parse_errors` themselves.
    pub fn load_records(
        &self,
        folder: &Path,
        recursive: bool,
        verbose: bool,
    ) -> Result<LoadResult> {
        let files = self.list_files(folder, recursive)?;
        let mut records = Vec::new();
        let mut parse_errors = Vec::new();

        for path in files {
            match frontmatter::load_record(&path) {
                Ok(record) => records.push(record),
                Err(VaultdbError::NoFrontmatter(_)) => {
                    records.push(Record {
                        path: path.clone(),
                        fields: std::collections::BTreeMap::new(),
                        raw_content: None,
                    });
                }
                Err(VaultdbError::InvalidFrontmatter { file, reason }) => {
                    if verbose {
                        eprintln!("skipping (invalid frontmatter): {}: {}", file, reason);
                    }
                    parse_errors.push(crate::error::ParseError {
                        file: std::path::PathBuf::from(&file),
                        message: reason,
                    });
                }
                Err(e) => return Err(e),
            }
        }

        Ok(LoadResult { records, parse_errors })
    }

    /// Load records with raw content preserved (for write operations and link extraction),
    /// collecting per-file parse diagnostics.
    ///
    /// Files with no frontmatter are loaded as empty records with their raw content set.
    /// Files with invalid frontmatter are collected into `LoadResult.parse_errors` rather
    /// than dropped.
    pub fn load_records_with_content(
        &self,
        folder: &Path,
        recursive: bool,
        verbose: bool,
    ) -> Result<LoadResult> {
        let files = self.list_files(folder, recursive)?;
        let mut records = Vec::new();
        let mut parse_errors = Vec::new();

        for path in files {
            match frontmatter::load_record_with_content(&path) {
                Ok(record) => records.push(record),
                Err(VaultdbError::NoFrontmatter(_)) => {
                    let content = std::fs::read_to_string(&path)?;
                    records.push(Record {
                        path: path.clone(),
                        fields: std::collections::BTreeMap::new(),
                        raw_content: Some(content),
                    });
                }
                Err(VaultdbError::InvalidFrontmatter { file, reason }) => {
                    if verbose {
                        eprintln!("skipping (invalid frontmatter): {}: {}", file, reason);
                    }
                    parse_errors.push(crate::error::ParseError {
                        file: std::path::PathBuf::from(&file),
                        message: reason,
                    });
                }
                Err(e) => return Err(e),
            }
        }

        Ok(LoadResult { records, parse_errors })
    }

    /// Look up a single record by its filename (without the `.md` extension)
    /// inside the given folder.
    ///
    /// Returns `Ok(None)` if no such file exists. Returns `Ok(Some(record))`
    /// when the file exists and parses cleanly. Returns
    /// `Err(VaultdbError::InvalidFrontmatter)` if the file exists but its
    /// frontmatter is malformed — unlike `load_records`, single-record lookup
    /// surfaces parse errors as a hard error because the caller asked for one
    /// specific record.
    pub fn find_by_name(
        &self,
        folder: &str,
        name: &str,
    ) -> Result<Option<Record>> {
        let folder_path = self.resolve_folder(folder)?;
        let candidate = folder_path.join(format!("{}.md", name));
        if !candidate.is_file() {
            return Ok(None);
        }
        match frontmatter::load_record(&candidate) {
            Ok(record) => Ok(Some(record)),
            Err(VaultdbError::NoFrontmatter(_)) => Ok(Some(Record {
                path: candidate,
                fields: std::collections::BTreeMap::new(),
                raw_content: None,
            })),
            Err(e) => Err(e),
        }
    }

    /// Run a structured query against the vault. Returns the matching records,
    /// optionally projected, sorted, and limited per the `Query`'s fields.
    ///
    /// The records returned have `raw_content` set to `None` (use
    /// `load_records_with_content` if you need the body text).
    pub fn query(&self, q: &crate::query::Query) -> Result<Vec<Record>> {
        let folder_path = self.resolve_folder(&q.folder)?;

        // Determine if the filter references the link graph.
        let needs_links = q.filter.as_ref().map_or(false, expr_uses_links);

        // Load records with content if links are needed for extraction
        let load = if needs_links {
            self.load_records_with_content(&folder_path, q.recursive, false)?
        } else {
            self.load_records(&folder_path, q.recursive, false)?
        };
        let mut records = load.records;

        // Build a LinkIndex if the filter references the link graph.
        let link_index = if needs_links {
            Some(crate::links::LinkIndex::build(&records))
        } else {
            None
        };

        // Filter
        if let Some(filter) = &q.filter {
            records.retain(|r| {
                evaluate_expr(filter, r, &self.root, link_index.as_ref())
            });
        }

        // Sort
        if let Some(sort_key) = &q.sort {
            records.sort_by(|a, b| {
                let av = a.get(&sort_key.field, &self.root)
                    .unwrap_or(crate::record::Value::Null);
                let bv = b.get(&sort_key.field, &self.root)
                    .unwrap_or(crate::record::Value::Null);
                let ord = compare_values(&av, &bv);
                if sort_key.descending { ord.reverse() } else { ord }
            });
        }

        // Limit
        if let Some(limit) = q.limit {
            records.truncate(limit);
        }

        // Projection (if requested, keep only selected fields plus virtual fields)
        if let Some(select) = &q.select {
            let select_set: std::collections::BTreeSet<&str> =
                select.iter().map(|s| s.as_str()).collect();
            for record in records.iter_mut() {
                record.fields.retain(|k, _| select_set.contains(k.as_str()));
            }
        }

        Ok(records)
    }
}

// ---------------------------------------------------------------------------
// Query evaluation helpers (private to this module; Task 4 moves them to
// filter.rs once the refactor is complete).
// ---------------------------------------------------------------------------

/// Returns true if any node of `expr` references the link graph.
fn expr_uses_links(expr: &crate::query::Expr) -> bool {
    use crate::query::Expr;
    match expr {
        Expr::LinksTo(_) | Expr::LinkedFrom(_) => true,
        Expr::Predicate(_) => false,
        Expr::And(es) | Expr::Or(es) => es.iter().any(expr_uses_links),
        Expr::Not(e) => expr_uses_links(e),
    }
}

/// Evaluate an `Expr` against a single record.
fn evaluate_expr(
    expr: &crate::query::Expr,
    record: &Record,
    vault_root: &Path,
    link_index: Option<&crate::links::LinkIndex>,
) -> bool {
    use crate::query::{Expr, LinkPredicate};
    match expr {
        Expr::Predicate(p) => evaluate_predicate(p, record, vault_root),
        Expr::And(es) => es.iter().all(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Or(es) => es.iter().any(|e| evaluate_expr(e, record, vault_root, link_index)),
        Expr::Not(e) => !evaluate_expr(e, record, vault_root, link_index),
        Expr::LinksTo(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .outgoing_links(&record.virtual_name())
                .iter()
                .any(|n| *n == name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .outgoing_links(&record.virtual_name())
                .iter()
                .any(|target_name| {
                    idx.record_by_name(target_name)
                        .map_or(false, |target_record| {
                            evaluate_expr(inner, target_record, vault_root, Some(idx))
                        })
                }),
            (None, _) => false,
        },
        Expr::LinkedFrom(lp) => match (link_index, lp) {
            (Some(idx), LinkPredicate::Target(name)) => idx
                .incoming_links(&record.virtual_name())
                .iter()
                .any(|n| *n == name.as_str()),
            (Some(idx), LinkPredicate::Where(inner)) => idx
                .incoming_links(&record.virtual_name())
                .iter()
                .any(|source_name| {
                    idx.record_by_name(source_name)
                        .map_or(false, |source_record| {
                            evaluate_expr(inner, source_record, vault_root, Some(idx))
                        })
                }),
            (None, _) => false,
        },
    }
}

/// Evaluate a leaf `Predicate` against a single record.
fn evaluate_predicate(
    p: &crate::query::Predicate,
    record: &Record,
    vault_root: &Path,
) -> bool {
    use crate::query::{CompareOp, Predicate};
    use crate::record::Value;

    match p {
        Predicate::Equals { field, value } => {
            record.get(field, vault_root).as_ref() == Some(value)
        }
        Predicate::Contains { field, value } => match record.get(field, vault_root) {
            Some(Value::String(s)) => match value {
                Value::String(v) => s.contains(v.as_str()),
                _ => false,
            },
            Some(Value::List(list)) => list.iter().any(|item| item == value),
            _ => false,
        },
        Predicate::Compare { field, op, value } => {
            let actual = match record.get(field, vault_root) {
                Some(v) => v,
                None => return false,
            };
            let ord = compare_values(&actual, value);
            match op {
                CompareOp::Lt => ord == std::cmp::Ordering::Less,
                CompareOp::Le => ord != std::cmp::Ordering::Greater,
                CompareOp::Gt => ord == std::cmp::Ordering::Greater,
                CompareOp::Ge => ord != std::cmp::Ordering::Less,
                CompareOp::Ne => ord != std::cmp::Ordering::Equal,
            }
        }
        Predicate::Matches { field, regex } => match record.get(field, vault_root) {
            Some(Value::String(s)) => {
                regex::Regex::new(regex).map_or(false, |re| re.is_match(&s))
            }
            _ => false,
        },
        Predicate::StartsWith { field, value } => match record.get(field, vault_root) {
            Some(Value::String(s)) => s.starts_with(value.as_str()),
            _ => false,
        },
        Predicate::EndsWith { field, value } => match record.get(field, vault_root) {
            Some(Value::String(s)) => s.ends_with(value.as_str()),
            _ => false,
        },
        Predicate::Exists { field } => {
            !matches!(record.get(field, vault_root), None | Some(Value::Null))
        }
        Predicate::Missing { field } => {
            matches!(record.get(field, vault_root), None | Some(Value::Null))
        }
    }
}

/// Total order over `Value` for sorting. Mixed types fall back to debug-string
/// comparison so that sort is always stable.
fn compare_values(a: &crate::record::Value, b: &crate::record::Value) -> std::cmp::Ordering {
    use crate::record::Value;
    match (a, b) {
        (Value::Integer(x), Value::Integer(y)) => x.cmp(y),
        (Value::Float(x), Value::Float(y)) => {
            x.partial_cmp(y).unwrap_or(std::cmp::Ordering::Equal)
        }
        (Value::String(x), Value::String(y)) => x.cmp(y),
        (Value::Bool(x), Value::Bool(y)) => x.cmp(y),
        (Value::Null, Value::Null) => std::cmp::Ordering::Equal,
        (Value::Null, _) => std::cmp::Ordering::Less,
        (_, Value::Null) => std::cmp::Ordering::Greater,
        _ => format!("{:?}", a).cmp(&format!("{:?}", b)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    fn create_test_vault() -> TempDir {
        let dir = TempDir::new().unwrap();
        // Create .obsidian directory
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        // Create a notes folder
        fs::create_dir(dir.path().join("notes")).unwrap();
        // Create some .md files
        fs::write(
            dir.path().join("notes/test1.md"),
            "---\ntags:\n  - type/concept\nstatus: active\n---\nBody 1\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/test2.md"),
            "---\ntags:\n  - type/leaf\nstatus: draft\n---\nBody 2\n",
        )
        .unwrap();
        // A file without frontmatter
        fs::write(
            dir.path().join("notes/no_fm.md"),
            "# Just a heading\nNo frontmatter.\n",
        )
        .unwrap();
        // A non-md file (should be ignored)
        fs::write(dir.path().join("notes/readme.txt"), "not markdown").unwrap();
        dir
    }

    #[test]
    fn discover_vault_from_subfolder() {
        let dir = create_test_vault();
        let notes_dir = dir.path().join("notes");
        let vault = Vault::discover(&notes_dir).unwrap();
        assert_eq!(vault.root, dir.path());
    }

    #[test]
    fn discover_vault_not_found() {
        let dir = TempDir::new().unwrap();
        let result = Vault::discover(dir.path());
        assert!(matches!(result, Err(VaultdbError::VaultNotFound(_))));
    }

    #[test]
    fn resolve_folder_existing() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let path = vault.resolve_folder("notes").unwrap();
        assert_eq!(path, dir.path().join("notes"));
    }

    #[test]
    fn resolve_folder_missing() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let result = vault.resolve_folder("nonexistent");
        assert!(matches!(result, Err(VaultdbError::FolderNotFound(_))));
    }

    #[test]
    fn list_files_only_md() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let files = vault.list_files(&dir.path().join("notes"), false).unwrap();
        assert_eq!(files.len(), 3); // test1.md, test2.md, no_fm.md
        assert!(files.iter().all(|f| f.extension().unwrap() == "md"));
    }

    #[test]
    fn load_records_includes_no_frontmatter() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let records = vault
            .load_records(&dir.path().join("notes"), false, false)
            .unwrap()
            .records;
        // Should load all 3 .md files, including no_fm.md with empty fields
        assert_eq!(records.len(), 3);

        let no_fm = records
            .iter()
            .find(|r| r.virtual_name() == "no_fm")
            .unwrap();
        assert!(no_fm.fields.is_empty());
    }

    #[test]
    fn load_records_surfaces_invalid_frontmatter_as_parse_errors() {
        use std::fs;

        let dir = create_test_vault();
        // Add a file with malformed YAML frontmatter
        fs::write(
            dir.path().join("notes/broken.md"),
            "---\n: : : not yaml\n---\nbody\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let result = vault
            .load_records(&dir.path().join("notes"), false, false)
            .unwrap();

        // The 3 valid-or-empty files (test1, test2, no_fm) load as records;
        // broken.md is collected as a parse error.
        assert_eq!(result.records.len(), 3);
        assert_eq!(result.parse_errors.len(), 1);
        assert!(result.parse_errors[0].file.ends_with("broken.md"));
        assert!(!result.parse_errors[0].message.is_empty());
    }

    #[test]
    fn recursive_listing() {
        let dir = create_test_vault();
        let sub = dir.path().join("notes/sub");
        fs::create_dir(&sub).unwrap();
        fs::write(
            sub.join("nested.md"),
            "---\ntags:\n  - type/concept\n---\nNested.\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let files_flat = vault.list_files(&dir.path().join("notes"), false).unwrap();
        let files_recursive = vault.list_files(&dir.path().join("notes"), true).unwrap();

        assert_eq!(files_flat.len(), 3);
        assert_eq!(files_recursive.len(), 4); // includes nested.md
    }

    #[test]
    fn find_by_name_existing() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let r = vault.find_by_name("notes", "test1").unwrap();
        assert!(r.is_some());
        assert_eq!(r.unwrap().virtual_name(), "test1");
    }

    #[test]
    fn find_by_name_missing() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let r = vault.find_by_name("notes", "no-such-record").unwrap();
        assert!(r.is_none());
    }

    #[test]
    fn find_by_name_no_frontmatter_loads_as_empty() {
        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        // create_test_vault() writes notes/no_fm.md with no frontmatter
        let r = vault.find_by_name("notes", "no_fm").unwrap().unwrap();
        assert!(r.fields.is_empty());
        assert_eq!(r.virtual_name(), "no_fm");
    }

    #[test]
    fn find_by_name_invalid_frontmatter_errors() {
        use std::fs;
        let dir = create_test_vault();
        fs::write(
            dir.path().join("notes/broken.md"),
            "---\n: : :\n---\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let result = vault.find_by_name("notes", "broken");
        assert!(matches!(
            result,
            Err(VaultdbError::InvalidFrontmatter { .. })
        ));
    }

    // ------------------------------------------------------------------
    // Vault::query tests (Task 3)
    // ------------------------------------------------------------------

    #[test]
    fn query_basic_filter() {
        use crate::query::{Expr, Predicate, Query};
        use crate::record::Value;

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        // create_test_vault() writes test1.md (status: active) and
        // test2.md (status: draft), plus no_fm.md (no frontmatter).
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Equals {
                field: "status".into(),
                value: Value::String("active".into()),
            })),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };

        let results = vault.query(&q).unwrap();
        assert_eq!(results.len(), 1, "only test1 has status=active");
        assert!(results.iter().all(|r| {
            matches!(
                r.get("status", &vault.root),
                Some(Value::String(ref s)) if s == "active"
            )
        }));
    }

    #[test]
    fn query_with_limit_and_sort() {
        use crate::query::{Expr, Predicate, Query, SortKey};

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        // Exists predicate on _name matches all 3 records; limit cuts to 2.
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists { field: "_name".into() })),
            select: None,
            sort: Some(SortKey { field: "_name".into(), descending: false }),
            limit: Some(2),
            recursive: false,
        };

        let results = vault.query(&q).unwrap();
        assert!(results.len() <= 2, "limit must be respected");
        // Verify ascending sort: first element's name <= second's
        if results.len() == 2 {
            let a = results[0].virtual_name();
            let b = results[1].virtual_name();
            assert!(a <= b, "expected ascending order, got {:?} then {:?}", a, b);
        }
    }

    #[test]
    fn query_with_projection() {
        use crate::query::{Expr, Predicate, Query};

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        // Select only "status"; after projection every record's fields map
        // must contain only "status" (or be empty if the record had no status).
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists { field: "_name".into() })),
            select: Some(vec!["status".into()]),
            sort: None,
            limit: None,
            recursive: false,
        };

        let results = vault.query(&q).unwrap();
        // All 3 records are returned (no_fm.md has _name), but after projection
        // each record's frontmatter fields should only contain "status".
        assert!(!results.is_empty());
        let mut found_record_with_status = false;
        for r in &results {
            // Every record should have at most "status" in concrete fields
            assert!(
                r.fields.keys().all(|k| k == "status"),
                "expected only 'status' in fields, got {:?}",
                r.fields.keys().collect::<Vec<_>>()
            );
            if r.fields.contains_key("status") {
                found_record_with_status = true;
            }
        }
        // Some test record must actually have had "status" — otherwise we're testing nothing
        assert!(
            found_record_with_status,
            "expected at least one record to retain 'status' after projection"
        );
    }

    #[test]
    fn query_links_to_target() {
        use crate::query::{Expr, LinkPredicate, Query};
        use std::fs;

        let dir = create_test_vault();
        // Add a record that links to test1
        fs::write(
            dir.path().join("notes/linker.md"),
            "---\ntags:\n  - linker\n---\nLinks to [[test1]]\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::LinksTo(LinkPredicate::Target("test1".into()))),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };

        let results = vault.query(&q).unwrap();
        // Only `linker` links to test1
        let names: Vec<String> = results.iter().map(|r| r.virtual_name()).collect();
        assert!(
            names.contains(&"linker".to_string()),
            "expected linker, got {:?}",
            names
        );
    }
}
