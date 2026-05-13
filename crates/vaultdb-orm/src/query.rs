//! [`Query`]: a typed query builder over a [`Note`] type.
//!
//! Wraps `vaultdb_core::Query` so the user works with their typed model
//! instead of raw [`Record`]s. The `Note::discriminator` filter is
//! applied implicitly; user-supplied filters are AND-ed onto it.
//!
//! [`Record`]: vaultdb_core::Record

use std::marker::PhantomData;

use vaultdb_core::{Expr, Query as CoreQuery, SortKey, Vault};

use crate::error::{OrmError, Result};
use crate::note::Note;
use crate::update::Update;

/// A typed, chainable query builder.
///
/// Build with [`Query::new`] (or `T::query(&vault)` once Phase 3 lands).
/// Terminate with [`Query::fetch`], [`Query::first`], or [`Query::count`].
pub struct Query<'v, T: Note> {
    vault: &'v Vault,
    filter: Option<Expr>,
    /// True iff at least one user-supplied `.filter(...)` has been added.
    /// `T::discriminator()` alone does not flip this. Mutation entry
    /// points (`Query::update`) require this to be true so a typo'd or
    /// missing filter cannot accidentally update every record matching
    /// the discriminator.
    has_user_filter: bool,
    sort: Option<SortKey>,
    limit: Option<usize>,
    recursive: bool,
    _marker: PhantomData<fn() -> T>,
}

impl<'v, T: Note> Query<'v, T> {
    /// Start a new query for `T` against `vault`. The discriminator
    /// declared by `T` is applied automatically.
    pub fn new(vault: &'v Vault) -> Self {
        Self {
            vault,
            filter: T::discriminator(),
            has_user_filter: false,
            sort: None,
            limit: None,
            recursive: false,
            _marker: PhantomData,
        }
    }

    /// AND an additional [`Expr`] onto the current filter.
    pub fn filter(mut self, expr: Expr) -> Self {
        self.has_user_filter = true;
        self.filter = Some(match self.filter.take() {
            Some(existing) => match existing {
                // Flatten chained AND-of-AND so the AST stays shallow.
                Expr::And(mut clauses) => {
                    clauses.push(expr);
                    Expr::And(clauses)
                }
                other => Expr::And(vec![other, expr]),
            },
            None => expr,
        });
        self
    }

    /// Sort the result set by `sort`.
    pub fn order_by(mut self, sort: SortKey) -> Self {
        self.sort = Some(sort);
        self
    }

    /// Limit the result set to `n` records.
    pub fn limit(mut self, n: usize) -> Self {
        self.limit = Some(n);
        self
    }

    /// Recurse into subfolders.
    pub fn recursive(mut self, yes: bool) -> Self {
        self.recursive = yes;
        self
    }

    /// Materialise the query — runs against the vault and deserialises
    /// every matched record into `T`. A single record's parse failure
    /// short-circuits the whole call (the caller can no longer pretend
    /// a typed query succeeded if any row was the wrong shape).
    pub fn fetch(self) -> Result<Vec<T>> {
        let q = CoreQuery {
            folder: T::FOLDER.to_string(),
            filter: self.filter,
            select: None,
            sort: self.sort,
            limit: self.limit,
            recursive: self.recursive,
        };
        let records = self.vault.query(&q)?;
        records
            .iter()
            .map(|r| T::from_record(r, &self.vault.root))
            .collect()
    }

    /// Return the first matching record, or `None`.
    pub fn first(self) -> Result<Option<T>> {
        let mut hits = self.limit(1).fetch()?;
        Ok(hits.pop())
    }

    /// Count matching records. Currently materialises the result set;
    /// a streaming `query_iter`-based optimisation is a v0.2 candidate.
    pub fn count(self) -> Result<usize> {
        Ok(self.fetch()?.len())
    }

    /// Convert this query into an [`Update`] builder. Errors if no
    /// user-supplied `.filter(...)` has been added — the discriminator
    /// alone is not enough to authorise a bulk update, matching the
    /// safety stance of `vaultdb_core::UpdateBuilder` (which refuses
    /// unfiltered updates).
    pub fn update(self) -> Result<Update<'v, T>> {
        if !self.has_user_filter {
            return Err(OrmError::Custom(
                "Query::update() requires at least one .filter(...) — the model discriminator is not enough"
                    .into(),
            ));
        }
        let filter = self
            .filter
            .expect("filter set when has_user_filter is true");
        Ok(Update::new(self.vault, filter))
    }
}
