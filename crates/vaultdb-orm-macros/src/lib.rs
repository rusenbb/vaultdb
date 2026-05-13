//! Proc-macro support for `vaultdb-orm`.
//!
//! `#[derive(Note)]` reads `#[note(...)]` attributes on the struct and
//! emits:
//!
//! - An `impl vaultdb_orm::Note` block with `FOLDER`, optional
//!   `discriminator()`, optional `collection()`, and `field_names()`.
//! - One `pub fn <field>()` accessor per struct field, returning either
//!   a `FieldRef` (for plain frontmatter fields) or a `RelationRef`
//!   (for fields marked with `#[note(wikilink)]` / `#[note(backlink)]`).
//!   Plain fields honour `#[serde(rename = "...")]` so model authors
//!   can map onto virtual fields like `_name` without writing it twice.
//!
//! Supported struct-level keys:
//!
//! - `#[note(folder = "...")]` — vault folder for this model. Required
//!   UNLESS `discriminator` is given, in which case folder defaults to
//!   `""` (anywhere under the vault root) for tag-discriminated models.
//! - `#[note(discriminator = "...")]` — where-DSL filter parsed at
//!   runtime and applied implicitly to every query. Replaces and is
//!   preferred over the legacy `filter` alias below.
//! - `#[note(filter = "...")]` — legacy alias for `discriminator`. Both
//!   work; new code should use `discriminator`.
//! - `#[note(collection = "...")]` — name of the matching collection
//!   in `<vault>/vaultdb-schema.yaml`. When set, `Create::<T>::new`
//!   auto-resolves the schema (defaults + required-field enforcement)
//!   without an explicit `.with_schema(...)` call.
//!
//! Supported field-level keys:
//!
//! - `#[note(wikilink)]` — outgoing relation accessor, emits a
//!   `RelationRef` for filtering via `Expr::LinksTo`.
//! - `#[note(backlink)]` — incoming relation accessor (`Expr::LinkedFrom`).
//!
//! Unknown field-level `#[note(...)]` keys are a compile error so typos
//! like `#[note(wikilik)]` surface immediately.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Data, DeriveInput, Fields, LitStr, parse_macro_input};

#[proc_macro_derive(Note, attributes(note))]
pub fn derive_note(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    let name = input.ident.clone();

    let mut folder: Option<LitStr> = None;
    // `filter` is the legacy name for `discriminator` — both are
    // accepted, `discriminator` is preferred for new code.
    let mut filter: Option<LitStr> = None;
    let mut discriminator: Option<LitStr> = None;
    let mut collection: Option<LitStr> = None;

    for attr in &input.attrs {
        if !attr.path().is_ident("note") {
            continue;
        }
        let parsed = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("folder") {
                folder = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("filter") {
                filter = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("discriminator") {
                discriminator = Some(meta.value()?.parse()?);
            } else if meta.path.is_ident("collection") {
                collection = Some(meta.value()?.parse()?);
            } else {
                return Err(meta.error(
                    "unknown #[note(...)] key — expected `folder`, `filter`, `discriminator`, or `collection`",
                ));
            }
            Ok(())
        });
        if let Err(e) = parsed {
            return e.to_compile_error().into();
        }
    }

    // Folder defaults to "" (anywhere in vault) when a discriminator is
    // given — that's the eduport-style "tag-discriminated, host picks
    // the data folder" pattern.
    let folder_lit: LitStr = match (folder, &discriminator, &filter) {
        (Some(f), _, _) => f,
        (None, Some(_), _) | (None, _, Some(_)) => LitStr::new("", proc_macro2::Span::call_site()),
        (None, None, None) => {
            return syn::Error::new_spanned(
                &name,
                "missing required #[note(folder = \"...\")] or #[note(discriminator = \"...\")] on derive(Note)",
            )
            .to_compile_error()
            .into();
        }
    };

    // Prefer `discriminator` over `filter` when both are given. Either
    // produces the same `Note::discriminator()` impl.
    let discriminator_source = discriminator.or(filter);
    let discriminator_impl = match discriminator_source {
        Some(d) => quote! {
            fn discriminator() -> ::core::option::Option<::vaultdb_orm::Expr> {
                ::vaultdb_orm::Expr::parse(#d).ok()
            }
        },
        None => quote! {},
    };

    let collection_impl = match collection {
        Some(c) => quote! {
            fn collection() -> ::core::option::Option<&'static str> {
                ::core::option::Option::Some(#c)
            }
        },
        None => quote! {},
    };

    // Field accessors: one zero-argument fn per struct field, returning
    // a FieldRef tied to the resolved frontmatter key.
    let accessors = match field_accessors(&input.data) {
        Ok(toks) => toks,
        Err(e) => return e.to_compile_error().into(),
    };

    // Static slice of frontmatter field names this model declares —
    // used by `Note::field_names()` for schema-consistency helpers.
    // Skips relation-marker fields (`#[note(wikilink)]` / `backlink`).
    let field_name_strs = match field_name_strings(&input.data) {
        Ok(strs) => strs,
        Err(e) => return e.to_compile_error().into(),
    };
    let field_names_impl = quote! {
        fn field_names() -> &'static [&'static str] {
            &[ #( #field_name_strs ),* ]
        }
    };

    let expanded = quote! {
        impl ::vaultdb_orm::Note for #name {
            const FOLDER: &'static str = #folder_lit;
            #discriminator_impl
            #collection_impl
            #field_names_impl
        }

        impl #name {
            #accessors
        }
    };

    expanded.into()
}

fn field_name_strings(data: &Data) -> syn::Result<Vec<String>> {
    let fields = match data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            _ => return Ok(Vec::new()),
        },
        _ => return Ok(Vec::new()),
    };

    let mut out = Vec::new();
    for f in fields {
        let ident = match &f.ident {
            Some(i) => i,
            None => continue,
        };
        // Skip relation-marker fields — they aren't frontmatter.
        if relation_kind(f)?.is_some() {
            continue;
        }
        let key = serde_rename(f).unwrap_or_else(|| ident.to_string());
        // Skip the synthetic virtual field renames (`_name`, etc.) —
        // schema consistency cares about frontmatter, not virtuals.
        if key.starts_with('_') {
            continue;
        }
        out.push(key);
    }
    Ok(out)
}

fn field_accessors(data: &Data) -> syn::Result<proc_macro2::TokenStream> {
    let fields = match data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            Fields::Unnamed(_) => {
                return Err(syn::Error::new_spanned(
                    &s.fields,
                    "derive(Note) requires a struct with named fields",
                ));
            }
            Fields::Unit => return Ok(quote! {}),
        },
        Data::Enum(e) => {
            return Err(syn::Error::new_spanned(
                e.enum_token,
                "derive(Note) cannot be applied to an enum",
            ));
        }
        Data::Union(u) => {
            return Err(syn::Error::new_spanned(
                u.union_token,
                "derive(Note) cannot be applied to a union",
            ));
        }
    };

    let mut out = proc_macro2::TokenStream::new();
    for f in fields {
        let ident = match &f.ident {
            Some(i) => i,
            None => continue,
        };
        let accessor_ident = format_ident!("{}", ident);

        // Check for a relation attribute: #[note(wikilink)] / #[note(backlink)].
        match relation_kind(f)? {
            Some(RelationKind::Outgoing) => {
                out.extend(quote! {
                    pub fn #accessor_ident() -> ::vaultdb_orm::RelationRef {
                        ::vaultdb_orm::RelationRef::outgoing()
                    }
                });
                continue;
            }
            Some(RelationKind::Incoming) => {
                out.extend(quote! {
                    pub fn #accessor_ident() -> ::vaultdb_orm::RelationRef {
                        ::vaultdb_orm::RelationRef::incoming()
                    }
                });
                continue;
            }
            None => {}
        }

        let frontmatter_key = serde_rename(f).unwrap_or_else(|| ident.to_string());
        out.extend(quote! {
            pub fn #accessor_ident() -> ::vaultdb_orm::FieldRef {
                ::vaultdb_orm::FieldRef::new(#frontmatter_key)
            }
        });
    }

    Ok(out)
}

#[derive(Debug, Clone, Copy)]
enum RelationKind {
    Outgoing,
    Incoming,
}

/// Inspect a field's `#[note(...)]` attributes for `wikilink` or
/// `backlink` markers. Unknown `#[note(...)]` keys on a field are an
/// error (catches typos like `#[note(wikilik)]` early).
fn relation_kind(field: &syn::Field) -> syn::Result<Option<RelationKind>> {
    let mut kind: Option<RelationKind> = None;
    for attr in &field.attrs {
        if !attr.path().is_ident("note") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("wikilink") {
                kind = Some(RelationKind::Outgoing);
            } else if meta.path.is_ident("backlink") {
                kind = Some(RelationKind::Incoming);
            } else {
                return Err(meta.error(
                    "unknown #[note(...)] key on a field — expected `wikilink` or `backlink`",
                ));
            }
            Ok(())
        })?;
    }
    Ok(kind)
}

/// Extract `#[serde(rename = "...")]` from a field's attributes, if
/// present. Only the simple `rename = "x"` form is honoured; the
/// asymmetric `rename(serialize = .., deserialize = ..)` form is
/// ignored (frontmatter keys are bidirectional, so distinguishing them
/// would be a footgun).
fn serde_rename(field: &syn::Field) -> Option<String> {
    for attr in &field.attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let mut found: Option<String> = None;
        let _ = attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                if let Ok(s) = meta.value().and_then(|v| v.parse::<LitStr>()) {
                    found = Some(s.value());
                }
            } else {
                // Don't error on unknown keys — serde has many we don't care about.
                // Skip past their value so parse_nested_meta keeps walking.
                if meta.input.peek(syn::Token![=]) {
                    let _: syn::Result<syn::Expr> = meta.value().and_then(|v| v.parse());
                } else if meta.input.peek(syn::token::Paren) {
                    let _content;
                    let _ = syn::parenthesized!(_content in meta.input);
                    let _: proc_macro2::TokenStream = _content.parse().unwrap_or_default();
                }
            }
            Ok(())
        });
        if found.is_some() {
            return found;
        }
    }
    None
}
