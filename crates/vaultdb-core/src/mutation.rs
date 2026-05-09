//! Public typed mutation API for vault edits.
//!
//! Each builder provides:
//! - `plan(&self, vault) -> Result<MutationReport>` — read-only preview, never
//!   touches disk.
//! - `execute(self, vault) -> Result<MutationReport>` — apply the plan to disk
//!   and return the same report.
//!
//! The report shape is intentionally small: a vector of `PlannedChange` (path
//! + human-readable description) and a vector of `MutationError` for any
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

    /// Compute the report without writing.
    pub fn plan(&self, vault: &Vault) -> Result<MutationReport> {
        let (report, _writes) = self.compute(vault)?;
        Ok(report)
    }

    /// Plan, then apply each computed `WriteResult` to disk.
    pub fn execute(self, vault: &Vault) -> Result<MutationReport> {
        let (report, writes) = self.compute(vault)?;
        for w in &writes {
            writer::apply(w).map_err(VaultdbError::Io)?;
        }
        Ok(report)
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
            if !crate::filter::evaluate_expr(
                &self.filter,
                record,
                &vault.root,
                link_index.as_ref(),
            ) {
                continue;
            }

            let mut content = match &record.raw_content {
                Some(c) => c.clone(),
                None => {
                    errors.push(MutationError {
                        path: record.path.clone(),
                        message: "record has no raw_content; cannot apply update".into(),
                    });
                    continue;
                }
            };
            let original_content = content.clone();
            let mut wr_changes = Vec::new();
            let mut description_parts: Vec<String> = Vec::new();

            let result: Result<()> = (|| {
                for (field, value) in &self.set_fields {
                    let value_str = render_value_for_yaml(value);
                    let (new_content, change) =
                        writer::set_field(&content, field, &value_str)?;
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
        let builder = UpdateBuilder::new("notes", filter)
            .set("priority", Value::Integer(1));

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
}
