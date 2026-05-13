//! The [`Note`] trait — the contract a typed model implements (or has
//! generated via `#[derive(Note)]`) to be queryable through `vaultdb-orm`.

use std::path::Path;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value as JsonValue;
use vaultdb_core::{Expr, Record};

use crate::error::{OrmError, Result};
use crate::value::value_to_json;

/// A typed view onto a vault record.
///
/// Implementors declare:
/// - which folder their records live in ([`Note::FOLDER`]),
/// - an optional discriminator filter applied to every query
///   ([`Note::discriminator`]),
/// - how to materialise an instance from a [`Record`]
///   ([`Note::from_record`]) — a default implementation drives this
///   through serde, which is what `#[derive(Note)]` relies on.
pub trait Note: Sized + Serialize + DeserializeOwned {
    /// The folder, relative to the vault root, that holds records of
    /// this type. Empty (`""`) means "anywhere under the vault root" —
    /// useful when the type is discriminated by tag rather than folder,
    /// and the host application picks the data folder at runtime.
    const FOLDER: &'static str;

    /// An optional filter applied implicitly to every query of this
    /// type. The natural place to discriminate when many record kinds
    /// share a folder (e.g. `tags contains type/paper`).
    ///
    /// Default: no discriminator.
    fn discriminator() -> Option<Expr> {
        None
    }

    /// Name of the collection in `<vault>/vaultdb-schema.yaml` that
    /// declares this model's shape. When set, `Create::<T>::new` and
    /// other consumers can auto-resolve the matching `CollectionSchema`
    /// for default-application and required-field checks.
    ///
    /// Default: `None` — auto-resolution disabled; callers attach a
    /// schema manually via `.with_schema(...)` if they want it.
    fn collection() -> Option<&'static str> {
        None
    }

    /// Field names this model declares in frontmatter (minus
    /// relations). Used by consistency-check helpers — defaults to an
    /// empty slice for hand-written impls; `#[derive(Note)]` generates
    /// the right list automatically.
    fn field_names() -> &'static [&'static str] {
        &[]
    }

    /// Parse a [`Record`] into `Self`.
    ///
    /// The default implementation builds a JSON object out of the
    /// record's frontmatter plus the cheap path-derived virtual fields
    /// (`_name`, `_path`, `_folder`, `_modified`, `_created`) and feeds
    /// it to `serde_json::from_value`. Models can use `#[serde(rename
    /// = "_name")]` to map a typed field onto a virtual one without
    /// overriding this method.
    fn from_record(record: &Record, vault_root: &Path) -> Result<Self> {
        let json = record_to_json(record, vault_root);
        serde_json::from_value(json).map_err(OrmError::Deserialize)
    }
}

/// Build a JSON object from a record's frontmatter plus cheap virtual
/// fields. Exposed publicly because hand-written `from_record`
/// overrides often want to start from the same shape.
pub fn record_to_json(record: &Record, vault_root: &Path) -> JsonValue {
    let mut obj = serde_json::Map::new();

    for (k, v) in &record.fields {
        obj.insert(k.clone(), value_to_json(v));
    }

    // Cheap virtuals — always computable from path / fs metadata.
    // Accessed via Record::get so virtual_modified / virtual_created (which
    // are private on Record) flow through the public API.
    for key in ["_name", "_path", "_folder", "_modified", "_created"] {
        if let Some(v) = record.get(key, vault_root) {
            obj.insert(key.to_string(), value_to_json(&v));
        }
    }

    JsonValue::Object(obj)
}
