//! Read tools that don't touch the link graph: `query`, `find_by_name`,
//! `list_folders`. Implementations are free functions so they can be
//! unit-tested without spinning up the MCP runtime.

use rmcp::ErrorData;
use vaultdb_core::query::{Expr, Query, SortKey};
use vaultdb_core::record::Record;
use vaultdb_core::vault::Vault;

use crate::params::{FindByNameParams, ListFoldersParams, QueryParams};

/// Run a structured query against a folder. Returns the matching records
/// (with `raw_content` stripped to keep the wire format compact).
pub fn query(vault: &Vault, params: QueryParams) -> Result<Vec<Record>, ErrorData> {
    let filter = match params.r#where.as_deref() {
        Some(s) if !s.is_empty() => {
            Some(Expr::parse(s).map_err(|e| invalid("invalid where expression", e))?)
        }
        _ => None,
    };
    let select = params
        .select
        .as_deref()
        .map(|s| s.split(',').map(|f| f.trim().to_string()).collect());
    let sort = params.sort.map(|field| SortKey {
        field,
        descending: params.desc,
    });

    let q = Query {
        folder: params.folder,
        filter,
        select,
        sort,
        limit: params.limit,
        recursive: params.recursive,
    };

    vault
        .query(&q)
        .map(strip_raw_content)
        .map_err(|e| invalid("query failed", e))
}

/// Single-record lookup. Returns `null` when the record doesn't exist
/// (caller can distinguish "not found" from "error").
pub fn find_by_name(vault: &Vault, params: FindByNameParams) -> Result<Option<Record>, ErrorData> {
    vault
        .find_by_name(&params.folder, &params.name)
        .map(|opt| opt.map(strip_one))
        .map_err(|e| invalid("find_by_name failed", e))
}

/// Walk the vault root and list direct subdirectories that contain
/// at least one `.md` file (those are the folders worth querying).
pub fn list_folders(vault: &Vault, _params: ListFoldersParams) -> Result<Vec<String>, ErrorData> {
    let mut folders = Vec::new();
    let entries = std::fs::read_dir(&vault.root).map_err(|e| invalid("read_dir failed", e))?;
    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        // Skip dotfolders (.obsidian, .trash, .git, ...).
        if entry
            .file_name()
            .to_str()
            .is_some_and(|s| s.starts_with('.'))
        {
            continue;
        }
        // Only include folders that contain at least one .md file (top-level only).
        let has_markdown = std::fs::read_dir(&path)
            .ok()
            .and_then(|d| {
                d.flatten()
                    .find(|e| e.path().extension().is_some_and(|ext| ext == "md"))
            })
            .is_some();
        if has_markdown && let Some(name) = path.file_name().and_then(|n| n.to_str()) {
            folders.push(name.to_string());
        }
    }
    folders.sort();
    Ok(folders)
}

// ── helpers ────────────────────────────────────────────────────────────────

fn strip_raw_content(records: Vec<Record>) -> Vec<Record> {
    records.into_iter().map(strip_one).collect()
}

fn strip_one(mut r: Record) -> Record {
    r.raw_content = None;
    r
}

fn invalid(context: &str, err: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_params(format!("{}: {}", context, err), None)
}
