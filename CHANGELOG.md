# Changelog

All notable changes to this project will be documented in this file. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

### Added
- `vaultdb-mcp` crate: a Model Context Protocol server (stdio) exposing
  vaultdb-core to LLM agents. 13 tools — `ping`, `query`, `find_by_name`,
  `list_folders`, `links`, `traverse`, `unresolved`, `schema_show`,
  `schema_infer`, plus four **plan-only** mutation tools: `plan_update`,
  `plan_delete`, `plan_move`, `plan_rename`. There are intentionally no
  `execute_*` tools — agents propose, hosts apply.
- `ARCHITECTURE.md`: workspace layout, library scope discipline rule,
  state boundary, public API contract, plan/execute split, error
  handling, versioning policy.
- `crates/vaultdb-core/examples/bench.rs`: synthetic benchmark
  (`cargo run --release -p vaultdb-core --example bench -- <N>`).
  README's perf table now quotes measured numbers from this example.
- `&&` operator in the where-DSL, with SQL-style precedence
  (`||` binds tighter than `&&`).
- `pub const GRAPH_VIRTUAL_FIELDS` listing the four graph virtual
  field names; both vaultdb-core and vaultdb (CLI) source from it.
- End-to-end smoke test for `vaultdb-mcp` driving the actual binary
  over JSON-RPC.
- `[workspace.package]` inheritance for `version` / `edition` /
  `license` / `repository` across all member crates.

### Changed
- `vaultdb-core`'s public `VaultdbError` no longer leaks
  `serde_yaml::Error` (the `Yaml` variant is gone). Errors mapped to
  `SchemaError` / `InvalidFrontmatter` with a human-readable reason.
  `serde_yaml` stays an internal dependency; consumers no longer
  transitively depend on it.
- `LinkGraph` fields are now private (the spec said "private fields"
  but they were `pub`). Use the existing accessor methods.
- `Direction` (the public enum) is used end-to-end. The internal
  `TraverseDirection` mirror is gone. CLI sites now import `Direction`
  directly.
- `compare_values` now compares Integer/Float on a common float scale
  rather than falling through to debug-string ordering. Sort and
  `Compare` predicate evaluation are now correct across mixed numeric
  types.
- README repositioned around the library: lead reframes vaultdb as
  "Markdown vaults, queryable everywhere you want to use them" and
  introduces the workspace table; new "Library usage (vaultdb-core)"
  and "MCP server (vaultdb-mcp)" sections with copy-pasteable examples.

### Fixed
- **Real correctness bug:** `expr_uses_links` only inspected
  `LinksTo`/`LinkedFrom` variants, missing predicates that referenced
  graph virtual fields (`_link_count`, `_links`, `_backlinks`,
  `_backlink_count`). `Vault::query` skipped the link-graph build for
  these, so `--where "_link_count > 0"` silently returned zero
  results. Fixed by walking predicate field names.
- **Real correctness bug:** CLI's `move` and `rename` commands had
  `for err in errors { anyhow::bail!(err) }` — `bail!` returned on the
  first iteration, dropping every subsequent error. Fixed by joining
  all error messages.
- `parse_frontmatter`'s YAML parser reason is now preserved in
  `InvalidFrontmatter` errors with the actual file path filled in.
  Previously the loader's `map_err(|_| ...)` wrote a useless
  "failed to parse YAML" placeholder regardless of the real cause.
- 8 deferred clippy lints from Phase 2b (collapsible_if,
  needless_range_loop, manual_strip, approx_constant) cleaned up.
- Stale doc comments referencing "temporary shim for Task 2" /
  "the existing internal type stays for now" updated to describe the
  long-term shape.

## Earlier history

This project is pre-1.0; prior to this Unreleased section, the workspace
went through three phases:

- **Phase 1 — workspace split.** Single binary crate refactored into a
  Cargo workspace with `vaultdb-core` (library) and `vaultdb` (CLI
  binary, name preserved so `cargo install vaultdb` keeps working).
  No public-API changes.
- **Phase 2a — foundation types.** `Record` / `Value` / `LoadResult` /
  `ParseError` / `VaultdbError` made public and `Serialize`/`Deserialize`-able.
  `serde_yaml::Value` removed from the public surface (`FieldSchema`'s
  `enum_values` is now `Vec<Value>`).
- **Phase 2b — query AST + mutation builders + LinkGraph.** Public
  `Expr` / `Predicate` / `LinkPredicate` / `Query` AST. `LinkGraph` (was
  `LinkIndex`) with `Direction` / `GraphScope` / `UnresolvedLink`.
  `UpdateBuilder` / `DeleteBuilder` / `MoveBuilder` / `RenameBuilder`
  with the `plan` / `execute` split. CLI migrated to use the public
  AST instead of the internal `WhereClause` type.
