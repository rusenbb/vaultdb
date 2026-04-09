use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::error::{Result, VaultdbError};
use crate::frontmatter;
use crate::record::Record;

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

    /// Load all records from a folder.
    /// Files without frontmatter are loaded as records with empty fields.
    pub fn load_records(
        &self,
        folder: &Path,
        recursive: bool,
        verbose: bool,
    ) -> Result<Vec<Record>> {
        let files = self.list_files(folder, recursive)?;
        let mut records = Vec::new();

        for path in files {
            match frontmatter::load_record(&path) {
                Ok(record) => records.push(record),
                Err(VaultdbError::NoFrontmatter(_)) => {
                    // Load as empty record — still queryable by virtual fields
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
                }
                Err(e) => return Err(e),
            }
        }

        Ok(records)
    }

    /// Load records with raw content preserved (for write operations and link extraction).
    /// Files without frontmatter are included with empty fields.
    pub fn load_records_with_content(
        &self,
        folder: &Path,
        recursive: bool,
        verbose: bool,
    ) -> Result<Vec<Record>> {
        let files = self.list_files(folder, recursive)?;
        let mut records = Vec::new();

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
                }
                Err(e) => return Err(e),
            }
        }

        Ok(records)
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
            .unwrap();
        // Should load all 3 .md files, including no_fm.md with empty fields
        assert_eq!(records.len(), 3);

        let no_fm = records
            .iter()
            .find(|r| r.virtual_name() == "no_fm")
            .unwrap();
        assert!(no_fm.fields.is_empty());
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
}
