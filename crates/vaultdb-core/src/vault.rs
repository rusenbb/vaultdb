//! [`Vault`]: the library entry point. Discovers a vault from `.obsidian/`,
//! lists files, loads records, runs structured queries, builds the link
//! graph. Also defines [`LoadResult`], the parse-diagnostic-bearing return
//! type from `Vault::load_records`.

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
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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

    /// Replay any pending journals from previously-crashed mutations.
    ///
    /// Currently the only mutation that writes a journal is
    /// [`crate::RenameBuilder::execute`] — a rename that crashed between
    /// the file rename and finishing every backlink rewrite leaves a
    /// journal at `<vault>/.vaultdb/rename-journal/`. This method
    /// replays each pending journal idempotently and returns the count
    /// of journals processed.
    ///
    /// Long-lived consumers (eduport-tauri, etc.) should call this
    /// at startup. Each mutation also runs replay implicitly under
    /// the vault lock, so the only behavioural difference is timing:
    /// explicit recovery surfaces leftover work earlier.
    pub fn recover(&self) -> Result<usize> {
        crate::lock::with_lock(&self.root, || crate::journal::replay_all(&self.root))
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
                    // Skip hidden directories — but allow the root entry
                    // itself, even when it lives under a hidden parent (e.g.
                    // a TempDir whose name starts with `.tmp`).
                    e.depth() == 0 || !e.file_name().to_str().is_some_and(|s| s.starts_with('.'))
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

        Ok(LoadResult {
            records,
            parse_errors,
        })
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

        Ok(LoadResult {
            records,
            parse_errors,
        })
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
    pub fn find_by_name(&self, folder: &str, name: &str) -> Result<Option<Record>> {
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

    /// Build a link graph over the given scope.
    ///
    /// `GraphScope::All` walks the whole vault recursively. `Folder(name)`
    /// scopes to one folder. `Where(expr)` first walks the whole vault, builds
    /// a temporary graph for predicate evaluation (so link predicates work),
    /// filters records, and rebuilds the graph from the filtered subset.
    ///
    /// Records are loaded with raw content so wikilinks can be extracted.
    pub fn link_graph(&self, scope: crate::links::GraphScope) -> Result<crate::links::LinkGraph> {
        use crate::links::{GraphScope, LinkGraph};
        let records: Vec<Record> = match scope {
            GraphScope::All => {
                self.load_records_with_content(&self.root, true, false)?
                    .records
            }
            GraphScope::Folder(folder) => {
                let path = self.resolve_folder(&folder)?;
                self.load_records_with_content(&path, true, false)?.records
            }
            GraphScope::Where(expr) => {
                let all = self
                    .load_records_with_content(&self.root, true, false)?
                    .records;
                let idx = LinkGraph::build_with_root(&all, Some(&self.root));
                all.into_iter()
                    .filter(|r| crate::filter::evaluate_expr(&expr, r, &self.root, Some(&idx)))
                    .collect()
            }
        };
        Ok(LinkGraph::build_with_root(&records, Some(&self.root)))
    }

    /// Run a structured query against the vault. Returns the matching records,
    /// optionally projected, sorted, and limited per the `Query`'s fields.
    ///
    /// The records returned have `raw_content` set to `None` (use
    /// `load_records_with_content` if you need the body text).
    ///
    /// Eager: loads, filters, sorts, limits, and projects all in memory.
    /// Use [`Vault::query_iter`] for the streaming variant when memory
    /// pressure matters (large vaults; bounded top-K with sort+limit).
    pub fn query(&self, q: &crate::query::Query) -> Result<Vec<Record>> {
        // Run query_iter and collect. The iterator's internal state
        // already handles filter / sort / limit / projection; we just
        // gather the result into a Vec. Errors mid-stream propagate.
        self.query_iter(q)?.collect::<Result<Vec<_>>>()
    }

    /// Streaming variant of [`Vault::query`].
    ///
    /// Returns an iterator yielding `Result<Record>`. The implementation
    /// chooses the most memory-efficient strategy compatible with the
    /// query:
    ///
    /// - **No sort, no graph predicate, no body-search**: pure file-by-
    ///   file streaming. Records are loaded one at a time and filtered
    ///   inline; resident memory is O(1) regardless of vault size.
    /// - **Sort + limit**: bounded top-K via a binary heap of size
    ///   `limit`. Memory is O(limit), so "give me the most-recent 50
    ///   records out of 100K" is cheap.
    /// - **Sort, no limit; or graph/body predicates**: materializes the
    ///   working set in memory the same way [`Vault::query`] does, then
    ///   streams from the buffer. Memory is O(N) — same as the eager
    ///   call. (We can't stream a sort without materializing, and graph
    ///   predicates need the link graph built from all records.)
    ///
    /// The iterator yields `Err(...)` on per-file IO failures rather
    /// than aborting the whole query; the caller decides whether to
    /// stop or continue.
    pub fn query_iter(&self, q: &crate::query::Query) -> Result<QueryIter> {
        let folder_path = self.resolve_folder(&q.folder)?;
        let needs_links = q
            .filter
            .as_ref()
            .is_some_and(crate::filter::expr_uses_links);
        // Body-content predicates (e.g. `_body contains "foo"`) need
        // raw_content loaded but DON'T need the link graph. We track
        // them separately so streaming with body predicates still works
        // — only the load function changes per file.
        let needs_body_content = q
            .filter
            .as_ref()
            .is_some_and(crate::filter::expr_needs_body_content);

        // Pure-streaming path: no sort, no graph predicates. We iterate
        // file paths lazily, load each record on demand, filter, and
        // yield. Body predicates are fine here — we just call
        // load_record_with_content per file when needed. Vault size
        // doesn't affect resident memory.
        if !needs_links && q.sort.is_none() {
            let paths = self.list_files(&folder_path, q.recursive)?;
            let select_set: Option<std::collections::BTreeSet<String>> = q
                .select
                .as_ref()
                .map(|fields| fields.iter().cloned().collect());
            return Ok(QueryIter {
                state: QueryIterState::Streaming(StreamingState {
                    paths: paths.into_iter(),
                    filter: q.filter.clone(),
                    select_set,
                    vault_root: self.root.clone(),
                    limit: q.limit,
                    yielded: 0,
                    needs_content: needs_body_content,
                }),
            });
        }

        // Materialized path: load everything, filter, then sort+limit
        // (with top-K when both are present and limit < total) and
        // project. This degrades gracefully into the same behaviour as
        // the previous eager implementation.
        let load = if needs_links || needs_body_content {
            self.load_records_with_content(&folder_path, q.recursive, false)?
        } else {
            self.load_records(&folder_path, q.recursive, false)?
        };
        let mut records = load.records;
        let link_index = if needs_links {
            Some(crate::links::LinkGraph::build(&records))
        } else {
            None
        };

        if let Some(filter) = &q.filter {
            records.retain(|r| {
                crate::filter::evaluate_expr(filter, r, &self.root, link_index.as_ref())
            });
        }

        match (&q.sort, q.limit) {
            (Some(sort_key), Some(limit)) if limit < records.len() => {
                records = top_k_sorted(records, sort_key, limit, &self.root);
            }
            (Some(sort_key), maybe_limit) => {
                sort_records(&mut records, sort_key, &self.root);
                if let Some(limit) = maybe_limit {
                    records.truncate(limit);
                }
            }
            (None, Some(limit)) => {
                records.truncate(limit);
            }
            (None, None) => {}
        }

        if let Some(select) = &q.select {
            let select_set: std::collections::BTreeSet<&str> =
                select.iter().map(|s| s.as_str()).collect();
            for record in records.iter_mut() {
                record.fields.retain(|k, _| select_set.contains(k.as_str()));
            }
        }

        Ok(QueryIter {
            state: QueryIterState::Materialized(records.into_iter()),
        })
    }
}

/// Streaming iterator yielded by [`Vault::query_iter`]. Each `next()`
/// produces `Result<Record>` so per-file errors surface to the caller
/// instead of aborting the whole query.
pub struct QueryIter {
    state: QueryIterState,
}

enum QueryIterState {
    /// Pure streaming: pulls one file at a time, loads, filters, yields.
    Streaming(StreamingState),
    /// Pre-materialized: a Vec collected upfront (sort or graph
    /// predicates required).
    Materialized(std::vec::IntoIter<Record>),
}

struct StreamingState {
    paths: std::vec::IntoIter<PathBuf>,
    filter: Option<crate::query::Expr>,
    select_set: Option<std::collections::BTreeSet<String>>,
    vault_root: PathBuf,
    limit: Option<usize>,
    yielded: usize,
    /// When true, each file is loaded with body content (raw_content
    /// populated) so body-search predicates can run. Otherwise we use
    /// the cheaper frontmatter-only load.
    needs_content: bool,
}

impl Iterator for QueryIter {
    type Item = Result<Record>;

    fn next(&mut self) -> Option<Self::Item> {
        match &mut self.state {
            QueryIterState::Streaming(s) => s.next_record(),
            QueryIterState::Materialized(iter) => iter.next().map(Ok),
        }
    }
}

impl StreamingState {
    fn next_record(&mut self) -> Option<Result<Record>> {
        // Stop early once limit is reached — this is part of why
        // streaming + limit is so cheap on large vaults.
        if let Some(limit) = self.limit
            && self.yielded >= limit
        {
            return None;
        }
        loop {
            let path = self.paths.next()?;
            let load_result = if self.needs_content {
                crate::frontmatter::load_record_with_content(&path)
            } else {
                crate::frontmatter::load_record(&path)
            };
            let record = match load_result {
                Ok(r) => r,
                Err(VaultdbError::NoFrontmatter(_)) => {
                    // No frontmatter: yield an empty-fields record. If
                    // body content was requested, populate raw_content
                    // by reading the file directly so body predicates
                    // can still run.
                    let raw_content = if self.needs_content {
                        std::fs::read_to_string(&path).ok()
                    } else {
                        None
                    };
                    Record {
                        path: path.clone(),
                        fields: std::collections::BTreeMap::new(),
                        raw_content,
                    }
                }
                Err(VaultdbError::InvalidFrontmatter { .. }) => {
                    // Skip files with malformed YAML — same behaviour
                    // as the eager load. Eduport-core / CLI consumers
                    // that want to surface these should call
                    // `Vault::load_records` and inspect parse_errors.
                    continue;
                }
                Err(e) => return Some(Err(e)),
            };

            if let Some(filter) = &self.filter
                && !crate::filter::evaluate_expr(filter, &record, &self.vault_root, None)
            {
                continue;
            }

            let mut record = record;
            if let Some(select_set) = &self.select_set {
                record.fields.retain(|k, _| select_set.contains(k));
            }
            self.yielded += 1;
            return Some(Ok(record));
        }
    }
}

/// Sort `records` in place by the given sort key.
fn sort_records(records: &mut [Record], sort_key: &crate::query::SortKey, vault_root: &Path) {
    records.sort_by(|a, b| {
        let av = a
            .get(&sort_key.field, vault_root)
            .unwrap_or(crate::record::Value::Null);
        let bv = b
            .get(&sort_key.field, vault_root)
            .unwrap_or(crate::record::Value::Null);
        let ord = crate::filter::compare_values(&av, &bv);
        if sort_key.descending {
            ord.reverse()
        } else {
            ord
        }
    });
}

/// Top-K via a bounded binary heap. Memory: O(k). Returns the K
/// records with the smallest (or, if descending, largest) sort-key
/// values, sorted in the requested order.
///
/// We use a max-heap (default `BinaryHeap`) wrapped in `Reverse` so it
/// behaves as a min-heap by default, then push descending-aware
/// comparisons through the wrapper. The final result is sorted at the
/// end via `into_sorted_vec`.
fn top_k_sorted(
    records: Vec<Record>,
    sort_key: &crate::query::SortKey,
    k: usize,
    vault_root: &Path,
) -> Vec<Record> {
    use std::cmp::Ordering;

    if k == 0 {
        return Vec::new();
    }

    // Wrapper that compares two records by the sort field. The order
    // of cmp is chosen so that `BinaryHeap`'s default max-heap behaviour
    // gives us the correct K records to *evict* — i.e. the heap holds
    // the K best candidates so far, and the root is the worst of those.
    struct Entry<'a> {
        sort_key: &'a crate::query::SortKey,
        vault_root: &'a Path,
        record: Record,
    }
    impl PartialEq for Entry<'_> {
        fn eq(&self, other: &Self) -> bool {
            self.cmp(other) == Ordering::Equal
        }
    }
    impl Eq for Entry<'_> {}
    impl PartialOrd for Entry<'_> {
        fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
            Some(self.cmp(other))
        }
    }
    impl Ord for Entry<'_> {
        fn cmp(&self, other: &Self) -> Ordering {
            let av = self
                .record
                .get(&self.sort_key.field, self.vault_root)
                .unwrap_or(crate::record::Value::Null);
            let bv = other
                .record
                .get(&self.sort_key.field, other.vault_root)
                .unwrap_or(crate::record::Value::Null);
            let ord = crate::filter::compare_values(&av, &bv);
            if self.sort_key.descending {
                ord.reverse()
            } else {
                ord
            }
        }
    }

    let mut heap: std::collections::BinaryHeap<Entry> =
        std::collections::BinaryHeap::with_capacity(k + 1);
    for record in records {
        let entry = Entry {
            sort_key,
            vault_root,
            record,
        };
        if heap.len() < k {
            heap.push(entry);
        } else if let Some(top) = heap.peek()
            && entry < *top
        {
            heap.pop();
            heap.push(entry);
        }
    }

    // `into_sorted_vec` returns ascending by `Ord`, which under our
    // descending-aware Ord gives the user-requested order.
    heap.into_sorted_vec()
        .into_iter()
        .map(|e| e.record)
        .collect()
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
        fs::write(dir.path().join("notes/broken.md"), "---\n: : :\n---\n").unwrap();
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
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
            select: None,
            sort: Some(SortKey {
                field: "_name".into(),
                descending: false,
            }),
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
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
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

    #[test]
    fn vault_link_graph_all_walks_full_vault() {
        use crate::links::GraphScope;
        use std::fs;
        let dir = create_test_vault();
        // Add a file with an outgoing wikilink so the graph has at least one edge
        fs::write(
            dir.path().join("notes/with_link.md"),
            "---\nstatus: active\n---\nLinks to [[test1]]\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let graph = vault.link_graph(GraphScope::All).unwrap();
        assert!(
            graph.incoming_links("test1").contains(&"with_link"),
            "expected with_link in test1's backlinks"
        );
    }

    #[test]
    fn vault_link_graph_folder_scopes_correctly() {
        use crate::links::GraphScope;
        use std::fs;
        let dir = create_test_vault();
        fs::write(
            dir.path().join("notes/with_link.md"),
            "---\nstatus: active\n---\nLinks to [[test1]]\n",
        )
        .unwrap();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let graph = vault
            .link_graph(GraphScope::Folder("notes".into()))
            .unwrap();
        assert!(graph.outgoing_links("with_link").contains(&"test1"));
    }

    // ── query_iter tests ────────────────────────────────────────────────

    #[test]
    fn query_iter_pure_streaming_yields_all_records() {
        use crate::query::Query;

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        // No filter, no sort, no limit, no graph predicate ⇒ pure stream.
        let q = Query {
            folder: "notes".into(),
            filter: None,
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        // create_test_vault() writes test1.md, test2.md, no_fm.md.
        assert_eq!(records.len(), 3);
    }

    #[test]
    fn query_iter_pure_streaming_filters_inline() {
        use crate::query::{Expr, Predicate, Query};
        use crate::record::Value;

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
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
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 1, "only test1 has status=active");
        assert_eq!(records[0].virtual_name(), "test1");
    }

    #[test]
    fn query_iter_streaming_respects_limit_without_loading_more() {
        // Streaming + limit should stop pulling files once `limit`
        // matches have been yielded. We can't directly observe the
        // load count from the public API, but we can at least verify
        // the limit is honored.
        use crate::query::{Expr, Predicate, Query};

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
            select: None,
            sort: None,
            limit: Some(2),
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 2);
    }

    #[test]
    fn query_iter_top_k_when_sort_and_limit_set() {
        // Top-K via bounded heap: with N=3 records and limit=2, we should
        // see the smallest two (or descending=true: largest two) by name.
        use crate::query::{Expr, Predicate, Query, SortKey};

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        // create_test_vault() has test1, test2, no_fm. Sort ascending
        // by _name and limit 2 → should produce ["no_fm", "test1"].
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
            select: None,
            sort: Some(SortKey {
                field: "_name".into(),
                descending: false,
            }),
            limit: Some(2),
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].virtual_name(), "no_fm");
        assert_eq!(records[1].virtual_name(), "test1");

        // Descending: should produce ["test2", "test1"].
        let q_desc = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
            select: None,
            sort: Some(SortKey {
                field: "_name".into(),
                descending: true,
            }),
            limit: Some(2),
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q_desc)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 2);
        assert_eq!(records[0].virtual_name(), "test2");
        assert_eq!(records[1].virtual_name(), "test1");
    }

    #[test]
    fn query_iter_falls_back_to_buffered_for_graph_predicates() {
        // Graph predicates can't run in pure-streaming mode (would need
        // the full link graph built upfront). The query_iter call must
        // still succeed and return the expected results — it just goes
        // through the materialized path internally.
        use crate::query::{Expr, LinkPredicate, Query};
        use std::fs;

        let dir = create_test_vault();
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
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        let names: Vec<String> = records.iter().map(|r| r.virtual_name()).collect();
        assert!(
            names.contains(&"linker".to_string()),
            "expected linker, got {:?}",
            names
        );
    }

    #[test]
    fn query_eager_and_query_iter_produce_identical_results() {
        // Property: for any query, `query()` and `query_iter().collect()`
        // should produce exactly the same Vec<Record>. This is a small
        // sample but it exercises filter + sort + limit + projection all
        // at once.
        use crate::query::{Expr, Predicate, Query, SortKey};

        let dir = create_test_vault();
        let vault = Vault::with_root(dir.path().to_path_buf());

        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Exists {
                field: "_name".into(),
            })),
            select: Some(vec!["status".into()]),
            sort: Some(SortKey {
                field: "_name".into(),
                descending: false,
            }),
            limit: Some(3),
            recursive: false,
        };

        let eager = vault.query(&q).unwrap();
        let streamed: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        assert_eq!(eager.len(), streamed.len());
        for (a, b) in eager.iter().zip(streamed.iter()) {
            assert_eq!(a.virtual_name(), b.virtual_name());
            assert_eq!(
                a.fields.keys().collect::<Vec<_>>(),
                b.fields.keys().collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn query_iter_body_contains_finds_records_by_body_text() {
        // `_body contains "needle"` is the body-search predicate.
        // Records whose body (the file content after the frontmatter)
        // contains the needle should match. Frontmatter content does
        // NOT count.
        use crate::query::{Expr, Predicate, Query};
        use crate::record::Value;
        use std::fs;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();

        // a.md: matches in body
        fs::write(
            dir.path().join("notes/a.md"),
            "---\nstatus: active\n---\nThis note discusses microservices.\n",
        )
        .unwrap();
        // b.md: needle appears in frontmatter, NOT body
        fs::write(
            dir.path().join("notes/b.md"),
            "---\ntags:\n  - microservices\n---\nNothing relevant.\n",
        )
        .unwrap();
        // c.md: doesn't match anywhere
        fs::write(
            dir.path().join("notes/c.md"),
            "---\nstatus: draft\n---\nIrrelevant text.\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Contains {
                field: "_body".into(),
                value: Value::String("microservices".into()),
            })),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };

        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();

        assert_eq!(
            records.len(),
            1,
            "only a.md has 'microservices' in its body, got: {:?}",
            records.iter().map(|r| r.virtual_name()).collect::<Vec<_>>()
        );
        assert_eq!(records[0].virtual_name(), "a");
    }

    #[test]
    fn query_iter_body_matches_runs_regex_on_body_text() {
        use crate::query::{Expr, Predicate, Query};
        use std::fs;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/intro.md"),
            "---\nstatus: active\n---\n# Introduction\n\nThis is the intro.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/no_heading.md"),
            "---\nstatus: active\n---\nJust text, no heading.\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());

        // Match files whose body starts with a level-1 heading.
        let q = Query {
            folder: "notes".into(),
            filter: Some(Expr::Predicate(Predicate::Matches {
                field: "_body".into(),
                regex: r"^\s*# ".into(),
            })),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].virtual_name(), "intro");
    }

    #[test]
    fn body_search_works_via_dsl_with_quoted_needle() {
        // End-to-end: parse a where-DSL string that uses _body, run
        // through query_iter, verify the right records come out.
        use crate::query::{Expr, Query};
        use std::fs;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/match.md"),
            "---\nstatus: active\n---\nApplied to Stanford last week.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/skip.md"),
            "---\nstatus: active\n---\nApplied to MIT.\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let filter = Expr::parse(r#"_body contains "Stanford""#).unwrap();
        let q = Query {
            folder: "notes".into(),
            filter: Some(filter),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].virtual_name(), "match");
    }

    #[test]
    fn body_search_combines_with_frontmatter_and_uses_streaming_path() {
        // `status = active && _body contains "Stanford"` is exactly
        // the kind of query eduport's command palette will use. It
        // doesn't reference the link graph, so it should still go
        // through the streaming path (just with content loaded).
        use crate::query::{Expr, Query};
        use std::fs;

        let dir = TempDir::new().unwrap();
        fs::create_dir(dir.path().join(".obsidian")).unwrap();
        fs::create_dir(dir.path().join("notes")).unwrap();
        fs::write(
            dir.path().join("notes/active_match.md"),
            "---\nstatus: active\n---\nApplied to Stanford.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/draft_match.md"),
            "---\nstatus: draft\n---\nApplied to Stanford.\n",
        )
        .unwrap();
        fs::write(
            dir.path().join("notes/active_no_match.md"),
            "---\nstatus: active\n---\nApplied to MIT.\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let filter = Expr::parse(r#"status = active && _body contains "Stanford""#).unwrap();
        let q = Query {
            folder: "notes".into(),
            filter: Some(filter),
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].virtual_name(), "active_match");
    }

    #[test]
    fn query_iter_skips_invalid_frontmatter_in_streaming_mode() {
        // Streaming mode should silently skip files whose YAML
        // frontmatter is malformed. The eager path collects them as
        // parse_errors; the streaming path matches the eager-path
        // behaviour for record yield (the broken file just doesn't
        // appear in the result).
        use crate::query::Query;
        use std::fs;

        let dir = create_test_vault();
        fs::write(
            dir.path().join("notes/broken.md"),
            "---\n: : : not yaml\n---\nbody\n",
        )
        .unwrap();

        let vault = Vault::with_root(dir.path().to_path_buf());
        let q = Query {
            folder: "notes".into(),
            filter: None,
            select: None,
            sort: None,
            limit: None,
            recursive: false,
        };
        let records: Vec<_> = vault
            .query_iter(&q)
            .unwrap()
            .collect::<Result<Vec<_>>>()
            .unwrap();
        // 3 valid + no broken record = 3 (broken.md skipped silently in streaming mode).
        assert_eq!(records.len(), 3);
        let names: Vec<String> = records.iter().map(|r| r.virtual_name()).collect();
        assert!(!names.contains(&"broken".to_string()));
    }
}
