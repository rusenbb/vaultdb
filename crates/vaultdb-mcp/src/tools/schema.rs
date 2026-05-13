//! Schema tools: `schema_show` and `schema_infer`.
//!
//! `schema_show` reads `<vault>/vaultdb-schema.yaml` and returns the
//! parsed schema. `schema_infer` walks a folder and returns the
//! auto-inferred collection schema as YAML; with `write = true` it also
//! merges the inferred collection into the persisted schema file.

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

pub fn schema_show(vault: &Vault, params: SchemaShowParams) -> Result<SchemaShowOutput, ErrorData> {
    let path = schema::schema_path(&vault.root);
    let mut full = schema::load_schema(&path).map_err(|e| {
        ErrorData::invalid_params(format!("loading {}: {}", path.display(), e), None)
    })?;

    if let Some(folder) = params.folder {
        let prefix = format!("{}/", folder);
        full.collections
            .retain(|_, c| c.folder == folder || c.folder.starts_with(&prefix));
    }

    Ok(SchemaShowOutput {
        path: path.display().to_string(),
        schema: full,
    })
}

/// Output of `schema_infer`: both the structured schema and a
/// rendered YAML form. When `write = true`, also reports the path the
/// schema was saved to.
#[derive(Debug, Serialize)]
pub struct SchemaInferOutput {
    pub folder: String,
    pub schema: CollectionSchema,
    pub yaml: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub written_to: Option<String>,
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

    // Render either the single inferred collection (preview) or the
    // merged full schema (when writing). The preview YAML is what an
    // agent shows the user before opting into `write`.
    let written_to = if params.write {
        let schema_path = schema::schema_path(&vault.root);
        let mut full = if schema_path.exists() {
            schema::load_schema(&schema_path).map_err(|e| {
                ErrorData::invalid_params(format!("loading existing schema: {}", e), None)
            })?
        } else {
            VaultSchema {
                collections: std::collections::BTreeMap::new(),
            }
        };
        full.collections
            .insert(params.folder.clone(), collection.clone());
        let merged_yaml = schema::schema_to_yaml(&full)
            .map_err(|e| ErrorData::invalid_params(format!("schema_to_yaml: {}", e), None))?;
        std::fs::write(&schema_path, &merged_yaml).map_err(|e| {
            ErrorData::invalid_params(format!("writing {}: {}", schema_path.display(), e), None)
        })?;
        Some(schema_path.display().to_string())
    } else {
        None
    };

    let preview = VaultSchema {
        collections: std::collections::BTreeMap::from([(
            params.folder.clone(),
            collection.clone(),
        )]),
    };
    let yaml = schema::schema_to_yaml(&preview)
        .map_err(|e| ErrorData::invalid_params(format!("schema_to_yaml: {}", e), None))?;

    Ok(SchemaInferOutput {
        folder: params.folder,
        schema: collection,
        yaml,
        written_to,
    })
}
