//! [`Update`]: a typed wrapper around `vaultdb_core::UpdateBuilder`.
//!
//! Reached from a `Query<T>` by calling `.update()`. Carries the
//! query's filter (which must include at least one user-added
//! `.filter(...)`, not only the model discriminator) and exposes
//! `.set(FieldRef, value)`, `.unset(FieldRef)`, `.add_tag`,
//! `.remove_tag`, the body ops (`.set_body`, `.append_body`,
//! `.clear_body`, `.body_separator`), and the plan/execute pair the
//! underlying core API provides.
//!
//! Like the core builder, mutations only happen on `.execute(...)`;
//! `.plan(...)` returns a read-only [`MutationReport`] preview.

use std::marker::PhantomData;

use vaultdb_core::schema::{self, VaultSchema};
use vaultdb_core::{Expr, MutationReport, UpdateBuilder as CoreUpdateBuilder, Value, Vault};

use crate::error::Result;
use crate::field::FieldRef;
use crate::note::Note;

pub struct Update<'v, T: Note> {
    vault: &'v Vault,
    inner: CoreUpdateBuilder,
    _marker: PhantomData<fn() -> T>,
}

impl<'v, T: Note> Update<'v, T> {
    /// Internal constructor. Use [`crate::Query::update`].
    pub(crate) fn new(vault: &'v Vault, filter: Expr) -> Self {
        let mut inner = CoreUpdateBuilder::new(T::FOLDER, filter);
        // Auto-attach the vault schema when this model opts in via
        // `T::collection()`. Mirrors `Create::new` so strict-write
        // enforcement is consistent across the typed surface.
        if T::collection().is_some() {
            let path = schema::schema_path(&vault.root);
            if path.is_file()
                && let Ok(s) = schema::load_schema(&path)
            {
                inner = inner.with_vault_schema(s);
            }
        }
        Self {
            vault,
            inner,
            _marker: PhantomData,
        }
    }

    /// Attach the vault-wide schema explicitly. Overrides whatever
    /// auto-attach happened in [`Self::new`] (or attaches when the
    /// model declined the auto-resolve gate).
    pub fn with_vault_schema(mut self, schema: VaultSchema) -> Self {
        self.inner = self.inner.with_vault_schema(schema);
        self
    }

    /// Set `field` to `value` for every matching record.
    pub fn set(mut self, field: FieldRef, value: impl Into<Value>) -> Self {
        self.inner = self.inner.set(field.name(), value.into());
        self
    }

    /// Remove `field` from every matching record.
    pub fn unset(mut self, field: FieldRef) -> Self {
        self.inner = self.inner.unset(field.name());
        self
    }

    /// Add `tag` to the `tags` list of every matching record.
    pub fn add_tag(mut self, tag: impl Into<String>) -> Self {
        self.inner = self.inner.add_tag(tag);
        self
    }

    /// Remove `tag` from the `tags` list of every matching record.
    pub fn remove_tag(mut self, tag: impl Into<String>) -> Self {
        self.inner = self.inner.remove_tag(tag);
        self
    }

    /// Replace the body of every matching record with `text`. Written
    /// verbatim. See [`vaultdb_core::UpdateBuilder::set_body`].
    pub fn set_body(mut self, text: impl Into<String>) -> Self {
        self.inner = self.inner.set_body(text);
        self
    }

    /// Append `text` to the body of every matching record, joined by
    /// the configured separator (default `"\n"`). Multiple calls
    /// accumulate. See [`vaultdb_core::UpdateBuilder::append_body`].
    pub fn append_body(mut self, text: impl Into<String>) -> Self {
        self.inner = self.inner.append_body(text);
        self
    }

    /// Clear the body of every matching record (frontmatter is
    /// preserved). See [`vaultdb_core::UpdateBuilder::clear_body`].
    pub fn clear_body(mut self) -> Self {
        self.inner = self.inner.clear_body();
        self
    }

    /// Override the separator between existing body and each
    /// appended chunk. See
    /// [`vaultdb_core::UpdateBuilder::body_separator`].
    pub fn body_separator(mut self, sep: impl Into<String>) -> Self {
        self.inner = self.inner.body_separator(sep);
        self
    }

    /// Preview the mutation without writing. Returns the same
    /// `MutationReport` shape `execute()` produces.
    pub fn plan(&self) -> Result<MutationReport> {
        Ok(self.inner.plan(self.vault)?)
    }

    /// Execute the mutation, writing the resulting files atomically
    /// (one tempfile-then-rename per affected record).
    pub fn execute(self) -> Result<MutationReport> {
        Ok(self.inner.execute(self.vault)?)
    }
}
