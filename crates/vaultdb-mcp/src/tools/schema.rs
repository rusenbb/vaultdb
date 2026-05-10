//! Schema tools: `schema_show` and `schema_infer`.
//!
//! `schema_show` reads `<vault>/vaultdb-schema.yaml` and returns the
//! parsed schema. `schema_infer` walks a folder and returns the
//! auto-inferred collection schema as YAML — it does not write to disk.

use rmcp::ErrorData;
use serde::Serialize;
use vaultdb_core::schema::{self, CollectionSchema, VaultSchema};
use vaultdb_core::vault::Vault;

use crate::params::{SchemaInferParams, SchemaShowParams};

/// Output of `schema_show`: the persisted schema, optionally filtered to
/// just the collections matching a folder.
#[derive(Debug, Serialize)]
pub struct SchemaShowOutput {
    pub path: String,
    pub schema: VaultSchema,
}

const SCHEMA_FILENAME: &str = "vaultdb-schema.yaml";

pub fn schema_show(vault: &Vault, params: SchemaShowParams) -> Result<SchemaShowOutput, ErrorData> {
    let path = vault.root.join(SCHEMA_FILENAME);
    let mut full = schema::load_schema(&path).map_err(|e| {
        ErrorData::invalid_params(format!("loading {}: {}", path.display(), e), None)
    })?;

    if let Some(folder) = params.folder {
        full.collections
            .retain(|_, c| c.folder == folder || c.folder.starts_with(&format!("{}/", folder)));
    }

    Ok(SchemaShowOutput {
        path: path.display().to_string(),
        schema: full,
    })
}

/// Output of `schema_infer`: both the structured schema and a
/// rendered YAML form (so the agent can hand it to the user verbatim).
#[derive(Debug, Serialize)]
pub struct SchemaInferOutput {
    pub folder: String,
    pub schema: CollectionSchema,
    pub yaml: String,
}

pub fn schema_infer(
    vault: &Vault,
    params: SchemaInferParams,
) -> Result<SchemaInferOutput, ErrorData> {
    let folder_path = vault
        .resolve_folder(&params.folder)
        .map_err(|e| ErrorData::invalid_params(format!("resolve_folder: {}", e), None))?;
    let load = vault
        .load_records(&folder_path, params.recursive, false)
        .map_err(|e| ErrorData::invalid_params(format!("load_records: {}", e), None))?;
    let collection = schema::infer_schema(&params.folder, &load.records);

    // Render a single-collection VaultSchema for the YAML output.
    let mut single = VaultSchema {
        collections: std::collections::BTreeMap::new(),
    };
    single
        .collections
        .insert(params.folder.clone(), clone_collection(&collection));
    let yaml = schema::schema_to_yaml(&single)
        .map_err(|e| ErrorData::invalid_params(format!("schema_to_yaml: {}", e), None))?;

    Ok(SchemaInferOutput {
        folder: params.folder,
        schema: collection,
        yaml,
    })
}

/// `CollectionSchema` doesn't derive `Clone` so we hand-clone for the
/// "render one and return one" pattern. Cheap given the size.
fn clone_collection(c: &CollectionSchema) -> CollectionSchema {
    CollectionSchema {
        description: c.description.clone(),
        folder: c.folder.clone(),
        filter: c.filter.clone(),
        required: c.required.clone(),
        fields: c
            .fields
            .iter()
            .map(|(k, v)| {
                (
                    k.clone(),
                    schema::FieldSchema {
                        field_type: v.field_type.clone(),
                        enum_values: v.enum_values.clone(),
                        min: v.min,
                        max: v.max,
                        required: v.required,
                    },
                )
            })
            .collect(),
    }
}
