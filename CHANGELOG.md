# Changelog

All notable changes to this project will be documented in this file. Format
follows [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [Unreleased]

## [1.6.1] — Typed ORM body surface + test coverage backfill

### Added

- **`orm::Update<T>` now exposes body ops.** `.set_body(text)`,
  `.append_body(text)`, `.clear_body()`, `.body_separator(sep)` —
  the typed wrapper now mirrors the v1.6.0 core surface so callers
  that go through `Query::<T>::update()` aren't forced to drop down
  to `vaultdb-core` to use the new feature.
- **`orm::Create<T>::body(text)`** — same treatment for the typed
  create wrapper. Overrides the template's body and the default
  `# {name}` placeholder.

### Tests

- Seven coverage gaps from the 1.6.0 diagnosis are now pinned: no-op
  body set is skipped (mtime-churn defense), append touches every
  matching record (per-record loop isolation), CRLF line endings on
  frontmatter survive a body write, appended `\n---\n` section break
  doesn't break re-parse, `plan()` description surfaces body ops,
  schema attached + body-only change passes validation, and the
  documented `clear → set → append` apply order holds regardless of
  builder call order.
- Three ORM body integration tests covering `Update<T>::set_body /
  append_body + body_separator / clear_body` and `Create<T>::body`.

### Docs

- CLI help for `--set-body`, `--append-body`, and `create --body`
  now states explicitly that body text is taken verbatim (escape
  sequences NOT interpreted). Suggests shell ANSI-C quoting
  (`$'...'`) when literal newlines are needed. Only
  `--body-separator` interprets escapes — clarifying the asymmetry
  surfaced by the v1.6.0 review.

## [1.6.0] — Body writes: append / overwrite / clear; create with body

### Added

- **Body mutations on `UpdateBuilder`.** Three new builder methods cover the
  full edit surface for the markdown body (everything after the closing `---`
  of the frontmatter):
  - `.set_body(text)` — overwrite the body verbatim.
  - `.append_body(text)` — append, joined to the existing body by a
    caller-chosen separator. Default `"\n"` (compact join, idempotent against
    files that already end with a newline); override via
    `.body_separator("\n\n")` for a blank-line section break.
  - `.clear_body()` — drop the body entirely; frontmatter is preserved.
  All three play cleanly with frontmatter ops in the same call — a single
  `execute()` can re-tag a record AND log an entry to its body in one atomic
  write.
- **`CreateBuilder::body(text)`.** Explicit body content for new notes.
  Overrides the template's body (frontmatter is still merged) and the default
  `# {name}` placeholder. Written verbatim.
- **CLI `update` body flags.** `--set-body`, `--append-body` (repeatable),
  `--clear-body`, `--body-separator` (with the usual `\n` / `\t` / `\r` /
  `\\` escape interpretation, since literal newlines are awkward in shells).
- **CLI `create --body`.** Body content for new notes from the command line.
- **MCP `plan_update` / `execute_update` body params.** `set_body`
  (`Option<String>`), `append_body` (`Vec<String>`), `clear_body` (`bool`),
  `body_separator` (`Option<String>`). Value is taken verbatim — no escape
  interpretation at the MCP boundary (clients can send `"\n"` literally in
  JSON if they want).
- **MCP `plan_create` / `execute_create` body param.** `body`
  (`Option<String>`) for setting the new note's body content.
- **`writer::set_body` / `writer::append_body` / `writer::clear_body`
  primitives.** The body-region counterparts to `set_field` / `add_tag` /
  `unset_field`. They preserve frontmatter byte-for-byte and the file's
  line-ending style. Bare files (no frontmatter delimiters) get an empty
  frontmatter synthesized, matching `set_field`'s behaviour.

### Behaviour notes

- Body op order within a single `UpdateBuilder` call is: clear → set →
  append. So `.clear_body().append_body("X")` is equivalent to
  `.set_body("X")`; `.set_body("X").append_body("Y")` produces `X{sep}Y`.
- `append_body` trims trailing `\n` / `\r` from the existing body before
  joining with the separator. The result is that repeated appends with the
  default `"\n"` separator don't accumulate blank lines, even if every
  appended chunk ends with its own newline.
- `set_body` writes the text verbatim — no auto-trailing-newline. Callers
  that want a final newline on disk should include it in the text.
- Records without valid frontmatter still don't match update filters (the
  loader requires frontmatter to materialize a record), so the bare-file
  synthesise path is mostly exercised by direct writer tests.

## [1.5.0] — Queryable body links: `_body_links` + element-wise list `matches`

### Added

- **`_body_links` virtual field.** Markdown links `[label](url)` in a note's
  body, exposed as a list of `{label, url}` maps — complements the wiki-link
  graph (`_links`, which captures `[[Note]]` links). Triggers body loading like
  `_body`.
- **`matches` works element-wise on list fields.** `FIELD matches REGEX` now
  returns true if the regex matches *any* element of a list (previously lists
  were unmatchable). Enables `_body_links matches "cs\.stanford\.edu"` and
  `tags matches "^topic/"`. Non-breaking — list `matches` was always `false`
  before.

## [1.4.0] — Writer initializes & retypes frontmatter; opt-in recursive mutations

### Added

- **Recursive mutations (opt-in).** `UpdateBuilder`, `DeleteBuilder`, and
  `MoveBuilder` gained `.recursive(bool)` (default `false`). The CLI's global
  `--recursive` flag and a new `recursive` parameter on the MCP
  `update`/`move`/`delete` tools now actually reach the mutation, so a single
  `--where` mutation can span a subtree of scattered records (e.g. `index`
  notes that live in topical folders). Previously `--recursive` was accepted
  but silently ignored on mutations — only files directly in the named folder
  were ever touched.

### Fixed

- **The writer can now initialize frontmatter on a bare file.** `set_field` /
  `add_tag` / `unset_field` previously failed with `NoFrontmatter` on a `.md`
  file that had no `---` block (e.g. a note created in Obsidian). The writer
  now synthesizes an empty frontmatter block and inserts into it, so an
  externally-authored note can be brought under schema management through
  vaultdb. A file that *opens* a frontmatter block but never closes it is
  still rejected as malformed.
- **A scalar can replace a block-style list/map field in a single set.**
  Setting a scalar over an existing block list/map previously refused
  ("complex type … use `--unset` first"); but for a *required* field stored
  as the wrong type, that unset would itself fail the required-field check —
  leaving no in-vaultdb path to repair the record. `set_field` now replaces
  the field's whole span with the new scalar. Flow-style lists (`[a, b]`) and
  multiline scalars (`|`, `>`) remain intentionally non-round-tripped.
- **No-op updates no longer rewrite files.** An `UpdateBuilder` set whose net
  result equals the on-disk content is now skipped — no reported change and
  no file rewrite, avoiding mtime churn.

## [1.3.1] — UpdateBuilder no longer double-quotes string scalars

### Fixed

- `UpdateBuilder::set(field, Value::String(_))` was running a two-layer
  quoting pass on every string value, producing a double-wrapped scalar
  on disk for any string containing a YAML-special character. Setting
  a URL like `https://www.amazon.com.tr/foo` landed as
  `url: "'https://www.amazon.com.tr/foo'"` (literal single quotes
  surrounded by double quotes) rather than the intended
  `url: 'https://www.amazon.com.tr/foo'`. When parsed back, the value
  was a string *containing the quote characters*, not the bare URL.

  Cause: `mutation::render_value_for_yaml` already calls
  `writer::quote_value` to produce a YAML-ready scalar, then handed the
  result to `writer::set_field`, which ran `yaml_quote_value` a second
  time. Because the value now contained `'`, the second pass wrapped it
  in double quotes.

  Fix: new `writer::set_field_preformatted` that writes its input
  verbatim (caller asserts the value is already a valid YAML scalar).
  `UpdateBuilder::compute` routes the scalar set path through it.
  `writer::set_field` (raw-string contract) is unchanged for its
  remaining direct callers.

  Affected paths in 1.3.0:
  - MCP `plan_update` / `execute_update` with `set_typed` containing
    any string value with `:`, `#`, `&`, `|`, etc.
  - CLI `update --set field=value` for the same character classes.
  - ORM `Update<T>::set` with typed `Value::String`.

- `yaml_quote_value` now also quotes **type-ambiguous bare scalars** —
  strings that, written without quotes, would parse as a different
  YAML type. Pre-1.3.1, `Value::String("true")` round-tripped through
  an update as the boolean `true`, and `Value::String("42")` as the
  integer `42`. After 1.3.1, both keep their string type — `'true'`
  and `'42'` on disk. Covers the YAML 1.1 truthy/null literals
  (`true / false / yes / no / on / off / null / ~`,
  case-insensitive) and anything that `f64::parse` accepts.

### Tests

- `mutation::tests::update_builder_writes_url_string_without_double_quoting`:
  end-to-end repro of the Bialetti / Amazon URL case. Asserts the
  on-disk shape is single-quoted and round-trips back through
  `Vault::load_records` as the bare URL.
- `mutation::tests::update_builder_preserves_string_that_looks_like_bool`:
  `Value::String("true")` round-trips as the string "true", not the
  boolean `true`.
- `writer::tests::set_field_preformatted_writes_value_verbatim`:
  defends the new function's contract — a pre-quoted input is not
  re-quoted.
- `writer::tests::set_field_still_quotes_raw_values`: confirms the
  public `set_field` still quotes raw strings exactly once (no
  regression on its existing contract).

## [1.3.0] — Strict whole-record schema enforcement on every write

The headline: if a `vaultdb-schema.yaml` exists, every create and
update validates the post-state of the record against **all
applicable collections** before writing. A note must satisfy a
catch-all *and* its specific collection simultaneously. Updates that
would leave the record in a violating state are blocked — including
pre-existing violations on unrelated fields. There is no
escape-hatch flag; repair is by direct `.md` edit.

### Added — `vaultdb-core::schema`

- `VaultSchema::applicable_collections(record_folder, projected,
  vault_root)` — picks every collection whose `folder` is `==` or an
  ancestor of `record_folder` AND whose `filter:` evaluates true
  against the projected record. Returns shallowest-folder first, so
  callers that layer defaults in iteration order get
  "deepest folder wins" for free.
- `validate_schema_consistency(schema)` — pairwise cross-collection
  checks run at schema load time. Catches:
  - **Tier 1, type conflict:** two folder-overlapping collections
    declaring the same field with different `type:` strings.
  - **Tier 1, default vs sibling:** a `default:` (or resolved
    `default_expr:`) that satisfies its own collection but violates
    a different overlapping collection's declaration of the same
    field — would silently break creates the moment the default
    fires.
  - **Tier 2, disjoint enums:** both collections declare non-empty
    `enum_values:` whose intersection is empty. Subset narrowing
    (e.g. `Notes.db-table = [movie, book, ...]` and
    `movies.db-table = [movie]`) stays allowed — it's the documented
    pattern.
  - **Tier 2, disjoint numeric ranges:** intersected `min`/`max` is
    empty.
- `filters_demonstrably_disjoint(a, b)` — skips the cross-checks
  above when the two collections' filters are mutually exclusive
  equality predicates on the same field (e.g.
  `db-table = movie` vs `db-table = book`), which is the pattern the
  schema uses to make sibling sub-collections never co-apply to a
  single record.
- `load_schema` now runs `validate_schema_consistency` after
  `validate_schema_defaults`, so a self-inconsistent schema fails
  fast instead of producing silent-broken writes later.

### Added — builder API

- **`CreateBuilder::with_vault_schema(VaultSchema)`** —
  attach the vault-wide schema instead of a single
  `CollectionSchema`. Inside `compute()` the builder picks
  applicable collections from a synthetic record built from
  post-template + post-set fields, layers defaults from each in
  shallowest-first order, then validates the final field map
  against *every* applicable collection. Violations are
  deduplicated on `(field, message)` so a catch-all and a
  sub-collection don't double-report the same missing required
  field.
- **`CreateBuilder::with_schema(CollectionSchema)`** retained as a
  back-compat shim — wraps a single collection in a one-entry
  `VaultSchema` and routes through the same code path.
- **`UpdateBuilder::with_vault_schema(VaultSchema)`** — new. The
  builder projects set/unset/add_tag/remove_tag onto a typed copy
  of `record.fields`, picks applicable collections against the
  projected record, and validates against each. Per-record batch
  isolation is preserved: a record whose projection violates is
  reported as a `MutationError` and skipped; the rest of the batch
  still writes.

### Changed — caller wiring

- **CLI** `create` and `update` commands attach the full vault
  schema (via `with_vault_schema`) whenever
  `<vault>/vaultdb-schema.yaml` exists. A malformed schema is a
  hard error rather than silently downgrading to no-enforcement.
- **MCP** `build_create` / `build_update` (and their `plan_*` /
  `execute_*` callers) attach via a new shared helper
  `load_vault_schema_opt(vault)`. `plan_update` now takes `&Vault`
  so it can load the schema.
- **ORM** `Create<T>::new` and `Update<T>::new` auto-attach the
  vault schema when the model declares `Note::collection()`. The
  ORM previously looked up one named collection by `T::collection()`
  and called `with_schema`; now it loads the whole schema so the
  catch-all and any other applicable collections also enforce.
  `Update<T>` gains a `with_vault_schema(...)` setter that mirrors
  `Create<T>`'s.

### Changed — write hardening

- **`atomic_create_with(path, content, opts)`** — new in
  `writer`. Same tempfile+rename pattern as `atomic_write_with`,
  but uses `persist_noclobber` so the rename refuses if the target
  already exists. `CreateBuilder::execute` switches to this
  variant, closing the TOCTOU window between the compute-time
  `dest.exists()` check and the rename — an external writer
  (Obsidian, vim, another tool that doesn't honour the vault lock)
  racing in cannot be silently clobbered.

### Tests

- `mutation::tests` gains 8 strict-enforcement cases:
  `update_rejects_type_mismatch`, `update_rejects_enum_violation`,
  `update_passes_when_unconstrained_field_changes`,
  `update_surfaces_preexisting_violation`,
  `update_skips_one_blocks_one_in_batch`,
  `update_validates_against_catchall_and_subfolder`,
  `create_rejects_type_mismatch`,
  `create_validates_against_multiple_applicable_collections`.
- `schema::tests` gains 8 consistency cases covering each Tier 1 /
  Tier 2 rule, the enum-narrowing positive case, the
  default-compatible-with-overlap positive case, and the
  filter-disjoint skip case modelled on the real-world `indexes` vs
  `archive` pair.
- `writer::tests::atomic_create_*` (2 new): refusal on existing
  target, success on new path.

### Migration notes

- Callers using `CreateBuilder::with_schema(CollectionSchema)`
  continue to work unchanged. The behaviour they observe gets
  stricter: any `type:` / `enum:` / `min`/`max` violation in the
  post-set field map now blocks the write, where 1.2.1 only
  enforced `required:`. If you were relying on permissive mode,
  surface a `MutationError` to the user rather than working around
  it — there is no flag to opt out.
- Schema files that contain genuine cross-collection contradictions
  (e.g. same field, disjoint enums on folders that overlap and
  filters that don't separate them) now fail to load. Fix the
  schema; `load_schema` returns a `SchemaError` naming the
  collections and the field.

## [1.2.1] — Typed list/map writes round-trip as block-style YAML

### Fixed

- `UpdateBuilder::set(field, Value::List(_))` and `Value::Map(_)` used to
  flatten the typed value through `mutation::render_value_for_yaml`,
  producing a single-line YAML string (e.g. `"- kedi"`) which the
  line-oriented `writer::set_field` then wrote as a quoted scalar
  (`anlamlar: '- kedi'`). The field's typed shape was lost — a subsequent
  `Vault::query` read back `Value::String("- kedi")` instead of
  `Value::List([...])`.

  After 1.2.1, lists and maps go through a new
  `writer::set_field_block` that emits proper block-style YAML across
  multiple lines. The same flow-style and multiline-scalar refusals as
  `set_field` are preserved when replacing an existing complex field.
  Scalars still take the original `set_field` path.

- This affected every typed write path added in 1.1.0:
  - MCP `plan_update` / `execute_update` with `set_typed` containing
    lists or maps.
  - ORM `Create<T>` and `UpdateBuilder` calls in Rust passing
    `Value::List` / `Value::Map`.

  `CreateBuilder` was unaffected — it renders the whole frontmatter via
  `serde_yaml::to_string(&fields)`, which already emits block-style YAML
  for nested collections. The legacy CLI string-set path (`--set
  field=value`) was also unaffected — it cannot produce a typed list in
  the first place.

### Tests

- `writer::tests::set_field_block_*` (6 new): insert as block, multi-item
  round-trip, replace existing block list, map as nested mapping,
  flow-style refusal, scalar-value guard.
- `mutation::tests::update_builder_writes_list_as_block_yaml`:
  end-to-end regression — set a `Value::List` via `UpdateBuilder`,
  re-read through `Vault::load_records`, assert the field comes back
  typed as `Value::List`, not flattened.

## [1.2.0] — Vault-scoped export for CLI and MCP

The headline: every read path can now save its results to a file under
the vault root. One renderer in `vaultdb-core::render`, three call
sites (CLI, MCP, the future bindings), and the same path-safety rules
everywhere.

### Added — `vaultdb-core::render`

- New public module. Public API: `Format::{Csv { delimiter }, Xlsx,
  Json, Yaml}`, `Format::from_path` for extension inference,
  `resolve_export_path` (vault-scoped sandbox + auto-suffix on
  collision), `export_records` for record-shaped data, `export_value`
  for arbitrary serde-serializable values.
- Re-exported from the crate root as `ExportFormat`, `export_records`,
  `export_value`, `resolve_export_path`.
- New `xlsx` Cargo feature, default off. Pulls in `rust_xlsxwriter`
  only when enabled. The CLI and MCP binaries enable it; wasm / pyo3
  bindings choose.
- `csv` is now a `vaultdb-core` direct dep (was previously only in the
  CLI). Its dep tree is small enough to be on by default.

### Path safety

- All exports land **inside the vault root**. Absolute paths and `..`
  components are rejected at the boundary; symlink escapes are caught
  by canonicalize-parent + `starts_with(canonical_vault)`.
- `.md` is rejected — that's the vault's note format, not an export
  format.
- On filename collision the renderer auto-suffixes `(1)`, `(2)`, ...
  No overwrite mode, by design — agents shouldn't be able to clobber
  vault notes via a misaimed export path.
- Writes are atomic (tempfile-in-same-dir + rename), same shape as
  `writer::atomic_write`.

### Added — CLI: `vaultdb query --output <path>`

- New global flag on the `query` subcommand. Path is vault-relative,
  format inferred from extension (`.csv`, `.tsv`, `.json`, `.yaml`/
  `.yml`, `.xlsx`). When set, results are written to the file and the
  resolved path is printed; stdout no longer carries the result body.
- `--csv-delimiter {comma,semicolon,tab}` overrides the default for
  `.csv` and `.tsv` exports.
- The CLI no longer carries its own CSV/JSON/YAML formatters. `output.rs`
  shrank to just the comfy-table pretty-printer + a delegation shim
  into `vaultdb_core::render::render_records`.
- The CLI's direct `csv` dep is dropped — it reaches the writer through
  `vaultdb-core` now.

### Added — MCP: `export` parameter on all 8 read tools

- `query`, `find_by_name`, `list_folders`, `links`, `traverse`,
  `unresolved`, `schema_show`, `schema_infer` accept two new optional
  parameters via a flattened `ExportOptions`:
  - `export: "<vault-relative path>"`
  - `csv_delimiter: "comma" | "semicolon" | "tab"` (also accepts the
    literal characters `,` / `;` / `\t`)
- Response shape: when `export` is unset, tools return their existing
  shape (no change for current MCP clients). When `export` is set, the
  response is wrapped: `{ "data": <result>, "exported_to": "<path>" }`.
- Record-shaped tools (`query`, `find_by_name`) route through
  `render::export_records` to preserve virtual fields (`_name`,
  `_path`, `_backlink_count`, ...) — the file matches what
  `vaultdb query --output` would have written.
- Other tools route through `render::export_value`. JSON / YAML work
  for any shape; CSV / XLSX work for tabular shapes (array of objects,
  array of scalars, single object) and return a typed error otherwise.
- The vault is not modified — `export` is a read-only side effect that
  writes a fresh file inside the vault root. No new
  `--dangerously-allow-*` flag is needed.

### Removed

- XLS (the legacy binary Excel format pre-2007) is **not** supported.
  No maintained pure-Rust writer exists for it; FFI to a C/Java library
  would break the "pure-Rust, `cargo install` just works" property of
  the CLI. Anything that opens `.xls` in 2026 almost certainly also
  opens `.xlsx`.

### Tests

- 30 inline tests in `render.rs` cover extension inference per format,
  path-escape rejection (absolute, `..`, deep `..`), `.md` refusal,
  auto-suffix on collision, every format round-trip, CSV delimiter
  variants, XLSX magic-byte verification, and the three non-record
  shapes (array of objects, array of scalars, non-tabular refusal).
- 6 new MCP smoke tests cover the happy paths and rejection cases
  end-to-end through the JSON-RPC layer.

## [1.1.1] — Boolean literal support in CLI / DSL string paths

### Fixed

- `Value::parse_scalar` (record.rs) and `coerce_for_equals` (dsl.rs) now
  recognise the YAML bool literals `true` and `false` (case-sensitive)
  and produce `Value::Bool`. Previously both fell through to
  `Value::String("true")` / `Value::String("false")`, so:
  - `vaultdb update --set published=true` wrote a string instead of a
    YAML bool into frontmatter.
  - `vaultdb query --where "published = true"` compared
    `Value::String("true")` against the stored `Value::Bool(true)` —
    different enum variants, no match, empty result.
  - MCP `plan_update`'s legacy `set: ["field=true"]` form had the same
    issue.

  After 1.1.1, bool fields filter and round-trip correctly through
  every string-based interface. The typed JSON paths (MCP
  `plan_create` / `plan_update`'s `set_typed`, ORM `Create::<T>::set`)
  already handled bools correctly — this fix brings the string
  interfaces to parity.

- Case-sensitivity: only lowercase `true` / `false` coerce. Mixed-case
  (`True`, `FALSE`) stays a `Value::String`. Matches YAML's behaviour
  and avoids surprising consumers who actually want the string.

### Documentation

- README.md MCP section now lists `plan_create` and the five
  `execute_*` tools plus the four `--dangerously-allow-*` launch
  flags. The previous "intentionally no execute_* tools" claim was
  the 1.0 design and contradicted 1.1.0.
- ARCHITECTURE.md public API list adds `CreateBuilder`.
- RELEASE.md drops the `production` GitHub Environment approval
  step — that gate was removed from `publish.yml` in 1.1.0.
- vaultdb-orm/README.md examples use `discriminator` / `collection`
  attributes and demonstrate `Create<T>`. The legacy `filter` form
  still works and is mentioned as a backward-compatibility alias.

## [1.1.0] — Schema-aware create, MCP execute tools, ORM Create

The headline: vaultdb-core owns one create path that the CLI, MCP, and
ORM all share. Schemas in `vaultdb-schema.yaml` are now consulted at
create time, with defaults auto-filled and required fields enforced
before writing.

### Added — three new schema field types

- `wikilink` — string matching `[[name]]`, `[[name|alias]]`,
  `[[name#section]]`, `[[name#section|alias]]`. Shape validation only;
  target-existence checks are not in v1.
- `date` — string in `YYYY-MM-DD` with valid month/day ranges. Reuses
  the hand-rolled date arithmetic in `record::epoch_days_to_date`; no
  new date dependency.
- `url` — string parseable as an absolute URL via the `url` crate.
- New public helpers: `schema::is_valid_wikilink`, `is_valid_date`,
  `is_valid_url`.

### Added — schema defaults

- `FieldSchema::default: Option<Value>` — literal default applied at
  create time, validated against `field_type` and `enum_values` at
  schema load.
- `FieldSchema::default_expr: Option<String>` — dynamic default.
  Closed enum: `today` / `now` / `epoch`. Mutually exclusive with
  `default`.
- `schema::resolve_default_expr` resolves the keywords to `Value` at
  use time, via new `record::today_string` / `now_string` /
  `epoch_seconds` helpers.
- `load_schema` now calls `schema::validate_schema_defaults` after
  parsing, so bad defaults fail loudly with a `SchemaError` naming the
  collection and field.

### Added — CreateBuilder

- New `vaultdb_core::CreateBuilder` — fifth mutation builder, matching
  the `plan` / `execute` split of the existing four. Holds the
  vault-scoped lock during writes; writes are atomic via
  tempfile+rename.
- Composition order: template frontmatter → `--set` → schema defaults.
  Required-field check happens AFTER defaults, so missing required
  fields surface as `MutationError`s and the file is NOT written.
- `plan_with_content` returns both the report and the file content
  that would be written, so the CLI's `--dry-run` can show the
  resolved frontmatter.
- The CLI's `vaultdb create` is now schema-aware: when
  `<vault>/vaultdb-schema.yaml` exists with a collection matching the
  target folder, defaults auto-fill and required is enforced.

### Added — MCP `plan_create` + `execute_*` (flag-gated)

- `plan_create` MCP tool: schema-aware preview. Typed JSON `set` (no
  legacy `field=value` strings).
- `execute_create`, `execute_update`, `execute_delete`, `execute_move`,
  `execute_rename` MCP tools. **Disabled by default.** Enabled via
  launch flags:
  - `--dangerously-allow-create`
  - `--dangerously-allow-update` (covers update + move + rename)
  - `--dangerously-allow-delete` (soft-delete to `.trash/`)
  - `--dangerously-allow-permanent-delete` (required in addition to
    `--dangerously-allow-delete` for `permanent: true`)
- Every successful execute appends a line to
  `<vault>/.vaultdb/audit.log` (best-effort; doesn't block mutation).
- `plan_update` gained `set_typed: {field: value}` alongside the
  legacy `set: ["field=value"]` for forward compatibility with
  `plan_create`'s shape.

### Added — ORM `Create<T>`

- `vaultdb_orm::Create<T>` — typed wrapper around `CreateBuilder`,
  mirroring the existing `Update<T>`. Folder comes from `T::FOLDER`;
  typed `FieldRef` accessors give compile-checked sets.
- `Create::<T>::new` auto-resolves the matching `CollectionSchema`
  from `<vault>/vaultdb-schema.yaml` when `T::collection()` is `Some`
  — no explicit `.with_schema(...)` call needed.

### Added — `Note` trait + macro attributes

- New `Note::collection() -> Option<&'static str>` — names the YAML
  collection to bind to. Default `None`.
- New `Note::field_names() -> &'static [&'static str]` — the model's
  frontmatter field list (minus relations / virtual-mapped fields).
  Foundation for schema consistency tooling.
- `#[note(...)]` macro gained:
  - `discriminator = "..."` — preferred name for what was `filter`.
    Folder defaults to `""` (anywhere under vault root) when only a
    discriminator is given.
  - `collection = "..."` — name of the matching YAML collection.
- The legacy `filter` attribute still works as an alias for
  `discriminator`; no deprecation warning yet.

### Added — `schema init` writes the file

- `vaultdb schema init <folder> --write` merges the inferred
  collection into `<vault>/vaultdb-schema.yaml` (existing collections
  at the same folder are replaced; others preserved). Without
  `--write`, prints to stdout as before.
- MCP's `schema_infer` gained a matching `write: bool` parameter.

### Changed — internal cleanups (no behaviour change)

- `SCHEMA_FILENAME` constant and `schema_path()` helper hoisted to
  `vaultdb-core`; CLI and MCP no longer carry private copies.
- `Value::parse_scalar(&str)` hoisted to `vaultdb-core::record`;
  duplicate `parse_set_value` in CLI and MCP removed.
- `VaultSchema::collections_for_folder` and new
  `collection_for_folder` (exact match) replace the duplicated
  free function.
- `CollectionSchema`, `FieldSchema`, `VaultSchema` derive `Clone`;
  the hand-rolled `clone_collection` in MCP is gone.
- Removed dead `FieldSchema::required: Option<bool>` (collection-level
  `required: Vec<String>` is the sole source of truth).
- `[workspace.dependencies]` centralises `serde`, `serde_json`,
  `serde_yaml`, `anyhow`, `thiserror`, `tempfile`, `tracing`, `clap`,
  `url`.
- `UpdateBuilder::compute` now surfaces a missing `raw_content` as
  `VaultdbError::Internal` (was a per-record `MutationError` — but
  it's an invariant violation, not a soft error).

### Fixed — clippy 1.95 `collapsible_match`

- Restructured `validate_record`'s format-check block to use match
  arms with guards. CI was red on every push since the rust-toolchain
  pin picked up clippy 1.95.

### CI

- Removed the `production` GitHub Environment approval step from
  `publish.yml`. Tagging IS the approval — don't push tags from
  branches you don't intend to release.

## [0.4.0] — Query layer evolution

Phase B of the SQLite-of-markdown roadmap: a real DSL parser, a
streaming query API, and body-search predicates. The headline shape:
queries now scale gracefully past tens of thousands of records, the
DSL supports SQL-shaped expressions including parens and quoted
strings, and `_body contains "needle"` finds notes by their body
content.

### Added — body-search predicates (Phase B item 3)

- New `_body` virtual field on `Record::get_with_links` returning the
  full body text (everything after the closing `---` of the
  frontmatter). Works with all the existing operators:
  - `_body contains "search term"` — substring search
  - `_body matches "^# "` — regex search
  - `_body !contains "stale"` — negated substring
  - `_body startswith` / `_body endswith` — prefix/suffix anchors
- Body-search predicates are recognized by `expr_needs_body_content`,
  so the streaming query path automatically loads each record's
  raw_content when a body predicate is present and skips the load
  when none is needed.
- New `pub const BODY_VIRTUAL_FIELDS = ["_body", "_length",
  "_body_length"]` mirrors `GRAPH_VIRTUAL_FIELDS` and exposes the
  canonical list to consumers.

### Added — streaming query API (Phase B item 2)

- New `Vault::query_iter(&Query) -> Result<QueryIter>` returns an
  iterator over `Result<Record>` instead of materialising a Vec.
- Tiered implementation: pure file-by-file streaming when no sort
  and no graph predicates (O(1) memory regardless of vault size);
  bounded-heap top-K when sort + limit are both set with limit < N
  (O(limit) memory); buffered fallback for graph predicates and
  sort-without-limit (same cost as the eager path).
- `Vault::query` is now a thin wrapper over `query_iter().collect()`.

- **Real parser at `crate::dsl`** powered by `pest`. The grammar lives
  at `src/where_dsl.pest` and is the single source of truth.
- **Parenthesised grouping**: `(a = 1 || b = 2) && c = 3` now works.
- **Quoted string values**: `status = "in review"` and `'single
  quotes'`, with backslash escapes (`\"`, `\'`, `\\`, `\n`, `\t`).
- **`IN` and `NOT IN` operators**: `status IN (draft, active,
  "in review")` desugars to an OR of equalities; `NOT IN` is the
  negation. Quoted and unquoted values both work in the list.
- **`IS NULL` / `IS NOT NULL`**: SQL-conventional aliases for the
  existing `missing` / `exists` keywords. The legacy keywords still
  work.
- **Word-prefix `NOT`**: `NOT (status = active && hsk = 1)` for
  arbitrary-expression negation. The legacy `!`-attached negations
  (`!contains`, `!exists`, etc.) still work and are normalized into
  the same AST shape.
- **Improved parser error messages**: pest produces position-aware
  errors that pinpoint where the input stopped matching, e.g.
  `1:18 ^--- expected value`. Replaces the previous opaque
  `"no valid operator found"` message.

### Changed — DSL precedence (BREAKING within Phase B)

- **`AND` now binds tighter than `OR`**, matching SQL convention.
  In 0.3.0, `a || b && c` parsed as `(a || b) && c` (AND looser
  than OR — incorrect). It now parses as `a || (b && c)`. Existing
  `--where` strings that intended the AND-looser meaning need to
  add explicit parens; everything else is unaffected.

### Removed

- Internal `WhereClause` / `WhereExpr` AST and the string-split
  parser. The pest-driven parser produces `crate::query::Expr`
  directly; tests that poked the legacy types via reflection have
  been migrated to the public-API surface.

## [0.3.0] — Production-readiness pass

This release closes the correctness and safety gaps documented in the
SQLite-of-markdown audit. After v0.3.0, vaultdb-core is appropriate
for use as the data layer of a long-lived Tauri/desktop app.

### Added — concurrency, durability, recovery (Phase A)

- **Vault-scoped exclusive write lock.** Every mutation builder's
  `execute()` now acquires a flock-style lock at
  `<vault>/.vaultdb/lock` for the duration of its work. Concurrent
  mutations from any vaultdb-core consumer serialize cleanly. Uses
  `fs2` for cross-platform support (POSIX + Windows). Thread + process
  -level concurrency tests prove the lock works.
- **Atomic per-file writes via tempfile + rename.** New
  `writer::atomic_write` and `writer::atomic_write_with` write to a
  same-directory tempfile and rename over the target. Concurrent
  readers either see the full old or the full new content; never a
  partial write.
- **Crash-recovery journal for `RenameBuilder`.** Before any disk
  change, the builder writes a journal at
  `<vault>/.vaultdb/rename-journal/<timestamp>.json`. On crash, the
  next mutation (or an explicit `Vault::recover()` call) replays it
  idempotently and finishes the work. State machine handles
  rename-not-yet-done, rewrites-incomplete, and stale (both files
  gone) cases. New `crate::journal` module with full public surface.
- **Opt-in fsync via `WriteOptions { fsync: bool }`.** Each mutation
  builder gains `.write_options(opts)` and `.fsync(yes)` builders.
  When fsync=true: temp files are fsynced before rename; parent
  directories are fsynced after. After `execute()` returns, the
  change survives sudden power loss. Default off (matches
  pre-Phase-A behaviour). New `writer::fsync_dir` helper exposed.
- **`Vault::recover()`** method for explicit startup recovery.
  Long-lived consumers should call this at boot to finish any work
  left over from a previous crash.
- **`docs/SAFETY.md`** documenting every guarantee and non-guarantee:
  concurrency model, atomicity (per-file + multi-file via journal),
  durability contract, filesystem assumptions, recommended startup
  sequence.

### Added — vaultdb-mcp + ergonomics (Phase 3 of the rewrite spec)

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

### Added — error model

- `VaultdbError::Internal(String)` variant for vaultdb-internal
  invariant violations (e.g. journal serde failures). Bugs in
  vaultdb-core or unrecoverable filesystem situations, not user errors.

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
  types. **Cross-type non-numeric pairs** (String vs Integer, Bool vs
  List, etc.) now return `Ordering::Equal` and emit a `tracing::warn!`
  at the call site, instead of producing alphabetical-debug-string
  nonsense. Schema layer is the right place to enforce types.
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
