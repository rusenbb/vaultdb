# vaultdb architecture

## What vaultdb is

A markdown-as-database engine and the frontends that sit on it. Treats
folders of `.md` files with YAML frontmatter as queryable structured
data, with `[[wikilinks]]` forming a first-class link graph.

The thesis is that markdown vaults are *both* relational tables
(frontmatter is rows × columns) *and* graphs (wikilinks form edges), and
that a useful library treats both shapes equally — the query AST has
frontmatter predicates and link predicates as siblings in the same enum.

## Workspace layout

```
crates/
├── vaultdb-core/        the library: parse, links, query, mutate, schema
├── vaultdb/             the CLI binary, depends on vaultdb-core
└── vaultdb-mcp/         MCP server binary, depends on vaultdb-core
```

`vaultdb-core` is the only crate any external consumer needs. The CLI
and MCP server are reference frontends that exist in this repo as much
to validate the public API as to ship features — every change to
`vaultdb-core`'s public types is exercised by at least two consumers
that don't share assumptions, which is the test for a real library
boundary.

Future crates fit naturally without restructuring:
`vaultdb-server` (HTTP), `vaultdb-pyo3` (Python bindings),
`vaultdb-wasm` (browser/Tauri use). When a real second consumer shows
up for any of those, they get added as workspace members.

## Library scope discipline

> Every change to `vaultdb-core` must serve at least one consumer that
> is not an in-house application (eduport or otherwise). If you cannot
> describe a change without mentioning a specific application, the
> change belongs in that application's crate, not in `vaultdb-core`.
>
> Exception: parse-error and link-resolution edge cases discovered while
> building a specific consumer are bug fixes, not features. They apply
> to all consumers and stay in vaultdb.
>
> **Acceptance gate for new vaultdb-core features:** imagine a user of
> `vaultdb-mcp` (an LLM agent) or a third-party crate author building an
> unrelated markdown tool. Would they want this change? If the answer
> requires explaining one specific application's domain, the change is
> in the wrong layer.

This rule is what stops `vaultdb-core` from drifting into "the data
layer for one app, in Rust."

## State boundary

`vaultdb-core` holds **no mutable state** between calls. Every read
walks the filesystem fresh. There is no daemon, no cache, no state
files. This is a deliberate choice: the moment a library has a
long-running mutable cache, it has to ship a watcher and an
invalidation strategy, and now it owns concerns that are
application-shaped.

Stateful concerns — file watchers, full-text indexes, mutable in-memory
caches — belong in the consuming application. eduport (one such
consumer) holds an SQLite/FTS5 index and a `notify` watcher that
reconciles against `vaultdb-core` reads.

## Public API contract

### Stable types

The following are part of the public contract:

- `Vault` (`with_root`, `discover`, `load_records`, `find_by_name`,
  `query`, `link_graph`, `resolve_folder`)
- `Record` and `Value` (with `Serialize`/`Deserialize`)
- `Expr`, `Predicate`, `LinkPredicate`, `CompareOp`, `Query`, `SortKey`
- `LinkGraph`, `Direction`, `GraphScope`, `UnresolvedLink`
- `CreateBuilder`, `UpdateBuilder`, `DeleteBuilder`, `MoveBuilder`, `RenameBuilder` and
  their `MutationReport` / `PlannedChange` / `MutationError` types
- `render::Format` (re-exported as `ExportFormat`), `render::export_records`,
  `render::export_value`, `render::resolve_export_path`
- `LoadResult`, `ParseError`
- `VaultdbError` (the variants are part of the contract; only
  additions are non-breaking)

### Internals

Anything in modules whose public types are listed above but which isn't
itself listed above is internal. The `filter` module's `WhereClause`,
`WhereExpr`, and the where-DSL parser are `pub(crate)` for that reason.

### Mutation API: plan / execute split

Every mutation builder exposes both `plan(&self, &Vault) ->
Result<MutationReport>` and `execute(self, &Vault) ->
Result<MutationReport>`. `plan` produces a read-only preview; `execute`
runs the same computation and writes the result.

This shape is the dry-run guarantee lifted into the type system. It's
also what makes plan-only MCP tools possible: `vaultdb-mcp` exposes
only the `plan_*` calls; agents propose changes, humans approve, and
the host (CLI or Tauri or whatever) runs the corresponding `execute`.

### Graph predicates

`Expr::LinksTo(LinkPredicate)` and `Expr::LinkedFrom(LinkPredicate)` are
first-class AST variants. `LinkPredicate::Where(Box<Expr>)` is the
"join via links" primitive — "give me all notes that link to anything
matching this sub-expression."

Future graph features (shortest path, centrality, motif queries) become
new variants in the same enum and new methods on `LinkGraph` —
additive, not breaking.

## Error handling

`VaultdbError` is the public error type. It does not transitively
expose any underlying parser's error type — `serde_yaml::Error`,
`regex::Error`, etc., are wrapped into descriptive variants like
`InvalidFrontmatter` / `RegexError` / `SchemaError` rather than
re-exported. This keeps consumers free to pick a different YAML parser
or upgrade vaultdb's parser without breaking dependents.

Inside binaries (`vaultdb`, `vaultdb-mcp`), `anyhow::Error` is the
working error type and `VaultdbError` is converted at the boundary.

## Versioning

`vaultdb-core` is pre-1.0. Public-API breaking changes trigger a minor
bump. Once 1.0 ships, breaking changes follow semver.

The CLI binary's flag interface is also pre-1.0 and will stabilise
alongside the library.
