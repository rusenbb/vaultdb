//! Python bindings for `vaultdb-core`.
//!
//! Exposes a thin `vaultdb` Python module backed by PyO3. The
//! surface mirrors the most frequently-used pieces of the Rust
//! API — opening a vault, listing records in a folder, running
//! a where-DSL query, walking the link graph — without leaking
//! Rust types: every return value is a plain Python dict / list
//! / string so callers don't need a stub file to feel native.
//!
//! ## What's intentionally not exposed
//!
//! Mutation builders (`UpdateBuilder`, `DeleteBuilder`,
//! `RenameBuilder`, `MoveBuilder`) are out of scope for v1 of the
//! Python binding. Their builder pattern doesn't translate
//! cleanly into Python keyword args without losing type safety,
//! and the typical Python use-case (data-science workflow over a
//! vault) is read-heavy. Adding them later is an additive change.
//!
//! Streaming `query_iter` is also not exposed — the eager `query`
//! path is more idiomatic for Python (returns a list directly).
//! Streaming would require shipping a custom Iterator class.

use std::path::PathBuf;

use pyo3::exceptions::{PyIOError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::{PyDict, PyList};
use serde_json::Value as JsonValue;
use vaultdb_core::{Expr, GraphScope, Query, SortKey, Vault};

/// Convert a vaultdb error into a Python exception. Most errors
/// are mapped to `IOError` because vaultdb-core is fundamentally
/// a filesystem-backed library; parse failures map to `ValueError`
/// because they reflect malformed input.
fn vaultdb_error_to_py(err: vaultdb_core::VaultdbError) -> PyErr {
    use vaultdb_core::VaultdbError;
    match err {
        VaultdbError::InvalidFrontmatter { .. } | VaultdbError::NoFrontmatter(_) => {
            PyValueError::new_err(err.to_string())
        }
        _ => PyIOError::new_err(err.to_string()),
    }
}

/// Open a vault at `path` and confirm it's a directory. Returns
/// the resolved canonical path so the caller can use it with the
/// other functions (every other function takes the same path).
#[pyfunction]
fn open_vault(path: &str) -> PyResult<String> {
    let buf = PathBuf::from(path);
    if !buf.is_dir() {
        return Err(PyIOError::new_err(format!(
            "vault root is not a directory: {path}"
        )));
    }
    let vault = Vault::with_root(buf);
    // Resolve the folder root once — round-trips parse_errors etc.
    Ok(vault.root.to_string_lossy().into_owned())
}

/// Return the records in `folder` (relative to the vault root) as
/// a list of dicts shaped `{"path": str, "fields": dict}`. The
/// `fields` dict reflects the parsed YAML frontmatter; values are
/// JSON-converted (so YAML scalars become str/int/float/bool, and
/// nested mappings/sequences become dicts/lists).
#[pyfunction]
#[pyo3(signature = (vault_root, folder, recursive=false))]
fn list_records<'py>(
    py: Python<'py>,
    vault_root: &str,
    folder: &str,
    recursive: bool,
) -> PyResult<Bound<'py, PyList>> {
    let vault = Vault::with_root(PathBuf::from(vault_root));
    let folder_path = vault.resolve_folder(folder).map_err(vaultdb_error_to_py)?;
    let load = vault
        .load_records(&folder_path, recursive, false)
        .map_err(vaultdb_error_to_py)?;

    let result = PyList::empty(py);
    for record in load.records {
        let item = PyDict::new(py);
        item.set_item("path", record.path.to_string_lossy().to_string())?;
        item.set_item("fields", fields_to_py(py, &record.fields)?)?;
        result.append(item)?;
    }
    Ok(result)
}

/// Run a where-DSL query against `folder`. Returns a list of dicts
/// (same shape as `list_records`). Optional `limit` caps results.
#[pyfunction]
#[pyo3(signature = (vault_root, folder, where_clause=None, sort=None, limit=None))]
fn query<'py>(
    py: Python<'py>,
    vault_root: &str,
    folder: &str,
    where_clause: Option<&str>,
    sort: Option<&str>,
    limit: Option<usize>,
) -> PyResult<Bound<'py, PyList>> {
    let vault = Vault::with_root(PathBuf::from(vault_root));

    let filter = match where_clause {
        Some(w) => Some(Expr::parse(w).map_err(vaultdb_error_to_py)?),
        None => None,
    };
    let sort_key = match sort {
        Some(s) => Some(parse_sort_key(s)?),
        None => None,
    };

    let q = Query {
        folder: folder.to_string(),
        filter,
        select: None,
        sort: sort_key,
        limit,
        recursive: false,
    };
    let records = vault.query(&q).map_err(vaultdb_error_to_py)?;

    let result = PyList::empty(py);
    for record in records {
        let item = PyDict::new(py);
        item.set_item("path", record.path.to_string_lossy().to_string())?;
        item.set_item("fields", fields_to_py(py, &record.fields)?)?;
        result.append(item)?;
    }
    Ok(result)
}

/// Walk the vault's wikilink graph and return a dict of
/// `{ "incoming": {name: [src...]}, "outgoing": {name: [dst...]} }`.
/// Useful for "what links to X" queries without needing to think
/// about Rust references on the Python side.
#[pyfunction]
fn link_graph<'py>(py: Python<'py>, vault_root: &str) -> PyResult<Bound<'py, PyDict>> {
    let vault = Vault::with_root(PathBuf::from(vault_root));
    let graph = vault
        .link_graph(GraphScope::All)
        .map_err(vaultdb_error_to_py)?;
    // LinkGraph keys by note name. To enumerate names we walk the
    // vault once and let the graph answer in/out per note. This is
    // O(N) where N is record count and matches what the CLI does.
    let load = vault
        .load_records(&vault.root, true, false)
        .map_err(vaultdb_error_to_py)?;

    let result = PyDict::new(py);
    let incoming = PyDict::new(py);
    let outgoing = PyDict::new(py);

    for record in &load.records {
        let name = record.virtual_name();
        let inbound = graph
            .incoming_links(&name)
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        if !inbound.is_empty() {
            incoming.set_item(&name, inbound)?;
        }
        let outbound = graph
            .outgoing_links(&name)
            .into_iter()
            .map(String::from)
            .collect::<Vec<_>>();
        if !outbound.is_empty() {
            outgoing.set_item(&name, outbound)?;
        }
    }
    result.set_item("incoming", incoming)?;
    result.set_item("outgoing", outgoing)?;
    Ok(result)
}

/// Parse the `sort` argument shape `"<field>"` or
/// `"<field> asc"` / `"<field> desc"`. The bare field defaults to
/// ascending. Returns a typed `SortKey` so the caller doesn't have
/// to know about it.
fn parse_sort_key(s: &str) -> PyResult<SortKey> {
    let trimmed = s.trim();
    if trimmed.is_empty() {
        return Err(PyValueError::new_err("sort key cannot be empty"));
    }
    let (field, descending) = match trimmed.rsplit_once(char::is_whitespace) {
        Some((f, dir)) => match dir.to_ascii_lowercase().as_str() {
            "asc" => (f.trim().to_string(), false),
            "desc" => (f.trim().to_string(), true),
            // Unrecognised trailing word — treat the whole thing as
            // a field name (allows fields with spaces, though those
            // are rare in YAML keys).
            _ => (trimmed.to_string(), false),
        },
        None => (trimmed.to_string(), false),
    };
    Ok(SortKey { field, descending })
}

/// Convert a vaultdb `Record::fields` map into a Python dict. The
/// value type tower goes through serde_json so YAML's complex
/// scalar types collapse to a familiar Python value tower
/// (str / int / float / bool / list / dict / None).
fn fields_to_py<'py>(
    py: Python<'py>,
    fields: &std::collections::BTreeMap<String, vaultdb_core::Value>,
) -> PyResult<Bound<'py, PyDict>> {
    let dict = PyDict::new(py);
    for (k, v) in fields {
        let json: JsonValue = serde_json::to_value(v)
            .map_err(|e| PyValueError::new_err(format!("field {k:?} serialise: {e}")))?;
        dict.set_item(k, json_to_py(py, &json)?)?;
    }
    Ok(dict)
}

/// Recursive serde_json::Value → Python object converter. Plain
/// Python types only — no custom classes — so users never need a
/// type stub.
fn json_to_py<'py>(py: Python<'py>, v: &JsonValue) -> PyResult<Py<PyAny>> {
    // PyO3 0.27 returns `Bound<'py, PyBool>` for `bool::into_pyobject`;
    // its `.into_any()` consumes the borrow, but the borrow comes
    // from a reference (Python interns True/False), so clone it
    // first. Other primitives don't have this constraint.
    Ok(match v {
        JsonValue::Null => py.None(),
        JsonValue::Bool(b) => {
            let py_bool = b.into_pyobject(py)?;
            py_bool.to_owned().into_any().unbind()
        }
        JsonValue::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into_pyobject(py)?.into_any().unbind()
            } else if let Some(f) = n.as_f64() {
                f.into_pyobject(py)?.into_any().unbind()
            } else {
                n.to_string().into_pyobject(py)?.into_any().unbind()
            }
        }
        JsonValue::String(s) => s.as_str().into_pyobject(py)?.into_any().unbind(),
        JsonValue::Array(arr) => {
            let list = PyList::empty(py);
            for el in arr {
                list.append(json_to_py(py, el)?)?;
            }
            list.into_any().unbind()
        }
        JsonValue::Object(obj) => {
            let dict = PyDict::new(py);
            for (k, val) in obj {
                dict.set_item(k, json_to_py(py, val)?)?;
            }
            dict.into_any().unbind()
        }
    })
}

/// PyO3 module entry point. The module name comes from `lib.name`
/// in Cargo.toml (`vaultdb`). When maturin builds a wheel, the
/// resulting `.so` is named `vaultdb.cpython-3*.so` and CPython
/// resolves `import vaultdb` to it.
#[pymodule]
fn vaultdb(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_function(wrap_pyfunction!(open_vault, m)?)?;
    m.add_function(wrap_pyfunction!(list_records, m)?)?;
    m.add_function(wrap_pyfunction!(query, m)?)?;
    m.add_function(wrap_pyfunction!(link_graph, m)?)?;
    m.add("__version__", env!("CARGO_PKG_VERSION"))?;
    Ok(())
}
