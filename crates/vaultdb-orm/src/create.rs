//! [`Create`]: a typed wrapper around `vaultdb_core::CreateBuilder`.
//!
//! Reached via `Create::<T>::new(&vault, "name")`. Like
//! [`crate::Update`], it uses `T::FOLDER` from the `Note` trait so the
//! caller doesn't pass a folder string by hand, and accepts typed
//! [`FieldRef`] handles via `set(...)` so accessor typos are compile
//! errors.
//!
//! The schema lookup is opt-in via `with_schema(...)`. Phase 6 will
//! add a `collection` macro attribute on `#[derive(Note)]` that lets
//! `Create<T>` auto-resolve the right `CollectionSchema` from
//! `<vault>/vaultdb-schema.yaml`. For now, callers that want
//! default-application or required-field enforcement load the schema
//! themselves and hand the matching collection in.

use std::marker::PhantomData;

use vaultdb_core::schema::CollectionSchema;
use vaultdb_core::{CreateBuilder as CoreCreateBuilder, MutationReport, Value, Vault};

use crate::error::Result;
use crate::field::FieldRef;
use crate::note::Note;

pub struct Create<'v, T: Note> {
    vault: &'v Vault,
    inner: CoreCreateBuilder,
    _marker: PhantomData<fn() -> T>,
}

impl<'v, T: Note> Create<'v, T> {
    /// Start a typed create for `T` named `name` (the `.md` extension
    /// is appended automatically). Folder defaults to `T::FOLDER`.
    pub fn new(vault: &'v Vault, name: impl Into<String>) -> Self {
        Self {
            vault,
            inner: CoreCreateBuilder::new(T::FOLDER, name),
            _marker: PhantomData,
        }
    }

    /// Path to a template file, relative to the vault root.
    pub fn template(mut self, path: impl Into<String>) -> Self {
        self.inner = self.inner.template(path);
        self
    }

    /// Set a frontmatter field by typed accessor. Use this for fields
    /// declared on `T` — typos become compile errors.
    pub fn set(mut self, field: FieldRef, value: impl Into<Value>) -> Self {
        self.inner = self.inner.set(field.name(), value.into());
        self
    }

    /// Set a frontmatter field by raw key. Use this for fields not
    /// modelled on `T` (for example, a custom property your app
    /// declares in YAML but doesn't represent in the Rust struct).
    pub fn set_raw(mut self, key: impl Into<String>, value: impl Into<Value>) -> Self {
        self.inner = self.inner.set(key, value.into());
        self
    }

    /// Attach the collection schema for this folder. When set:
    /// `default:` / `default_expr:` fields auto-fill anything not
    /// supplied, and `required:` is enforced before writing.
    pub fn with_schema(mut self, schema: CollectionSchema) -> Self {
        self.inner = self.inner.with_schema(schema);
        self
    }

    /// Preview without writing. Returns the same `MutationReport`
    /// shape `execute()` produces.
    pub fn plan(&self) -> Result<MutationReport> {
        Ok(self.inner.plan(self.vault)?)
    }

    /// Plan and also return the file content that would be written.
    pub fn plan_with_content(&self) -> Result<(MutationReport, Option<String>)> {
        Ok(self.inner.plan_with_content(self.vault)?)
    }

    /// Execute: write the file atomically. Holds the vault-scoped
    /// lock for the duration.
    pub fn execute(self) -> Result<MutationReport> {
        Ok(self.inner.execute(self.vault)?)
    }
}
