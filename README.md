# vaultdb

[![crates.io](https://img.shields.io/crates/v/vaultdb.svg)](https://crates.io/crates/vaultdb)

**Markdown vaults, queryable everywhere you want to use them.** vaultdb is a Rust library for treating folders of `.md` files with YAML frontmatter as a queryable database, plus the frontends that sit on it: a CLI (`vaultdb`), an MCP server for LLM agents (`vaultdb-mcp`), and a stable library API (`vaultdb-core`) that any markdown-vault tool can build on.

The thesis: a markdown vault is *both* a relational table (frontmatter is rows × columns) *and* a graph (`[[wikilinks]]` are edges). vaultdb's query AST treats both as first-class — you filter records by frontmatter, by graph predicates ("links to anything tagged X"), or by any combination.

```rust
use vaultdb_core::{Expr, Predicate, Query, Value, Vault};

let vault = Vault::discover(std::path::Path::new("."))?;
let records = vault.query(&Query {
    folder: "notes".into(),
    filter: Some(Expr::Predicate(Predicate::Equals {
        field: "status".into(),
        value: Value::String("active".into()),
    })),
    select: None,
    sort: None,
    limit: Some(10),
    recursive: false,
})?;
```

Or from the CLI:

```
$ vaultdb query 3-Notes --where "tags contains topic/ai" --select "_name,_backlink_count" --sort _backlink_count --desc --limit 5

+-----------------------------+-----------------+
| _name                       | _backlink_count |
+==============================================+
| BERT                        | 43              |
| Machine Learning            | 39              |
| Transformer Architecture    | 38              |
| Natural Language Processing | 35              |
| Deep Learning               | 29              |
+-----------------------------+-----------------+
```

## What it does

- Treats folders of `.md` files as **database tables**
- YAML frontmatter fields are **queryable columns**
- `[[wiki-links]]` form a **citation graph** with backlink tracking
- Supports **relational joins** across the link graph
- **Graph traversal** (BFS) with depth limits and filtering
- **Bulk mutations** (set fields, add/remove tags) with `--dry-run` safety
- **Rename** with automatic wiki-link updates across the vault
- **Schema inference** and validation
- **Library + CLI + MCP server** in one workspace — pick whichever fits

No daemon, no cache, no state files. Every command reads the current `.md` files directly. Edit in Obsidian, query with vaultdb — they coexist without conflict.

## Workspace

| Crate | What it is | Use for |
|-------|------------|---------|
| `vaultdb-core` | Library: parse, query, link graph, mutation builders | Building a markdown-vault tool in Rust |
| `vaultdb` | CLI binary (this is what `cargo install vaultdb` ships) | Command-line use over an existing vault |
| `vaultdb-mcp` | Model Context Protocol server (stdio) | Letting LLM agents (Claude, Cursor, etc.) query a vault |

See [ARCHITECTURE.md](ARCHITECTURE.md) for the design rules these frontends follow (library scope discipline, state boundaries, public API contract).

## Install

```bash
# From crates.io
cargo install vaultdb

# Or from source
git clone https://github.com/rusenbb/vaultdb.git
cd vaultdb
cargo install --path .
```

Requires Rust 1.75+. Published at <https://crates.io/crates/vaultdb>.

## Quick start

```bash
# Auto-detects vault root by finding .obsidian/ directory
cd ~/Documents/my-vault

# Or specify explicitly
vaultdb --vault ~/Documents/my-vault query 3-Notes ...
```

## Data model

```
Folder             =  Database / Table
.md file           =  Record / Row
Frontmatter fields =  Columns
[[wiki-links]]     =  Relations / Edges
```

Every record automatically has virtual fields:

| Field | Description |
|-------|-------------|
| `_name` | Filename without `.md` |
| `_path` | Relative path from vault root |
| `_folder` | Parent folder name |
| `_modified` | File modification time |
| `_created` | File creation time |
| `_links` | Outgoing wiki-link targets |
| `_link_count` | Number of outgoing links |
| `_backlinks` | Notes that link to this note |
| `_backlink_count` | Number of incoming links |
| `_body` | The full body text (everything after the closing `---` of the frontmatter) — use with `contains`, `matches`, etc. for body search |
| `_length` | Total file size in bytes |
| `_body_length` | Body length in bytes (excluding frontmatter) |

## Commands

### Query

```bash
# Basic query with filtering, sorting, limiting
vaultdb query 3-Notes --where "tags contains topic/movies" --select "_name,director,year" --sort year --desc --limit 10

# Multiple --where flags are AND-ed
vaultdb query 3-Notes --where "tags contains topic/chinese" --where "hsk = 1" --select "_name,pinyin,anlam"

# OR within a single --where using ||
vaultdb query 3-Notes --where "status = to-watch || status = to-read" --select "_name,status"

# NOT with ! prefix
vaultdb query 3-Notes --where "tags !contains topic/movies" --where "status exists"

# Output formats: table (default), json, csv, yaml
vaultdb query 3-Notes --where "tags contains topic/ai" --format json
```

### Where expression syntax

```
FIELD = VALUE            # exact match
FIELD != VALUE           # not equal
FIELD > VALUE            # numeric/string comparison
FIELD < VALUE
FIELD >= VALUE
FIELD <= VALUE
FIELD contains VALUE     # list membership or substring
FIELD !contains VALUE    # negated
FIELD startswith VALUE
FIELD endswith VALUE
FIELD matches REGEX      # regex match
FIELD IN (a, b, c)       # SQL-style list membership
FIELD NOT IN (a, b, c)   # negated
FIELD IS NULL            # alias for `missing`
FIELD IS NOT NULL        # alias for `exists`
FIELD exists             # field is present and non-null
FIELD missing            # field is absent or null
FIELD !exists            # negated exists (same as missing)
```

Boolean composition inside a single `--where`:

```bash
# AND with && (SQL-conventional: binds tighter than ||)
--where "tags contains topic/ai && status = active"

# OR with ||
--where "status = active || status = pending"

# Mixed: AND binds tighter, so a || b && c parses as a || (b && c)
--where "status = draft || status = active && hsk = 1"

# Parenthesised grouping for explicit precedence
--where "(status = draft || status = active) && hsk = 1"

# Word-prefix NOT for negating a whole sub-expression
--where "NOT (status = archived || status = deleted)"

# Quoted string values for needles with spaces or special chars
--where 'title = "Two-word title"'
--where "status IN (\"in review\", \"needs follow-up\")"
```

Multiple `--where` flags are also AND-ed:

```bash
--where "status = active || status = pending" --where "tags contains topic/ai"
```

### Body search

Use the `_body` virtual field to search inside note bodies (the text after the frontmatter):

```bash
# Find every note that mentions "Stanford" in its body
vaultdb query 3-Notes --where '_body contains "Stanford"'

# Combined with frontmatter filtering — runs through the streaming
# query path, so it's cheap on large vaults.
vaultdb query 3-Notes --where 'status = active && _body contains "machine learning"'

# Regex on the body
vaultdb query 3-Notes --where '_body matches "^# Conclusion"'
```

Body content is loaded only when a body predicate is referenced; queries that don't need it stay on the fast frontmatter-only path.

### Create

```bash
# Create a note from a template
vaultdb create 3-Notes --template "templates/Movie Notes.md" --name "Arrival" --set "director=Denis Villeneuve" --set "year=2016"

# Create without a template (minimal frontmatter)
vaultdb create 3-Notes --name "Computer Vision" --set "tags=type/concept"

# Batch create from unresolved links
vaultdb unresolved 3-Notes --from BERT --depth 2 | ... | while read name; do
  vaultdb create 3-Notes --template templates/concept.md --name "$name"
done
```

The `--template` path is relative to vault root. Any `.md` file works as a template — vaultdb reads it, applies `--set` overrides to frontmatter, and writes the result.

### Count, Fields, Tags

```bash
# Count matching records
vaultdb count 3-Notes --where "tags contains topic/chinese"

# List all frontmatter fields with types and frequencies
vaultdb fields 3-Notes

# List all tags with usage counts
vaultdb tags 3-Notes
```

### Graph: Links, Traverse, Unresolved

```bash
# Show outgoing and incoming links for a note
vaultdb links React

# Find the most referenced notes
vaultdb query 3-Notes --select "_name,_backlink_count" --sort _backlink_count --desc --limit 10

# Find orphan notes (no links in or out)
vaultdb query 3-Notes --where "_backlink_count = 0" --where "_link_count = 0"

# BFS traversal from a starting note
vaultdb traverse Microservices --depth 2
vaultdb traverse Database --depth 1 --direction incoming

# Filter traversal results
vaultdb traverse BERT --depth 2 --where "tags contains type/concept" --select "_backlink_count"

# Find [[wiki-links]] pointing to non-existent files
vaultdb unresolved 3-Notes

# Scoped to a neighborhood
vaultdb unresolved 3-Notes --from BERT --depth 3

# Verbose: show which notes reference each unresolved link
vaultdb unresolved 3-Notes -v
```

### Relational joins

```bash
# Notes that link to React
vaultdb query 3-Notes --links-to React

# Notes that React links to
vaultdb query 3-Notes --linked-from React

# Notes linking to ANY note tagged topic/ai (the join)
vaultdb query 3-Notes --links-to-where "tags contains topic/ai" --select "_name,_backlink_count" --sort _backlink_count --desc

# Notes linked from any movie note
vaultdb query 3-Notes --linked-from-where "tags contains topic/movies" --where "tags contains type/concept"

# Notes linking to both React AND Node.js
vaultdb query 3-Notes --links-to React --links-to "Node.js" --select "_name"
```

### Mutations

All write operations support `--dry-run` to preview changes without writing.

```bash
# Set a field
vaultdb update 3-Notes --where "_name = 1917" --set "status=watched" --dry-run

# Add/remove tags
vaultdb update 3-Notes --where "director contains Chaplin" --add-tag "director/charlie-chaplin" --dry-run

# Remove a field
vaultdb update 3-Notes --where "_name = React" --unset "deprecated" --dry-run

# Move files
vaultdb move 5-Tasks --where "_name startswith 2026-02" --to 5-Tasks/archive --dry-run

# Delete (moves to .trash/ by default, --force for permanent)
vaultdb delete 3-Notes --where "_name = OldNote" --dry-run

# Rename with automatic wiki-link updates across the vault
vaultdb rename React "React.js" --folder 3-Notes --dry-run
```

Mutations require at least one `--where` condition to prevent accidental bulk changes.

### Schema

```bash
# Infer a schema from existing data
vaultdb schema init 3-Notes

# Validate records against a schema file (vaultdb-schema.yaml)
vaultdb schema validate 3-Notes

# Show the current schema
vaultdb schema show 3-Notes
```

When a `vaultdb-schema.yaml` exists at the vault root, `vaultdb create` will consult it for the target folder:

- Applies per-field `default:` and `default_expr:` (`today`, `now`, `epoch`) for fields not set by `--template` / `--set`
- Errors before writing if a required field is still missing

## Performance

No caching, no indexing — reads files fresh on every command.
Numbers below are best-of-3 from `cargo run --release --example bench -- <N>`,
measured on an Intel i7-14700K desktop (full host details and
methodology in **[BENCHMARKS.md](BENCHMARKS.md)**):

| Scale       | Frontmatter query | Graph query | `link_graph(All)` |
|-------------|------------------:|------------:|------------------:|
| 1 000 notes  |    5 ms |    7 ms |    6 ms |
| 10 000 notes |   59 ms |   88 ms |   70 ms |
| 100 000 notes |  651 ms | 1 032 ms |  819 ms |

Scaling is roughly linear in vault size — 10× the records costs
about 10–12× the time, with no superlinear cliff up through 100k.
Every operation finishes in **under 1.1 seconds at 100k notes**.

Reproduce with:

```bash
cargo run --release -p vaultdb-core --example bench -- 10000
```

Two-parser architecture: `serde_yaml` for fast reads, line-by-line string manipulation for formatting-preserving writes.

## Safety

- `--dry-run` previews all mutations before writing
- `update`, `move`, `delete` refuse to run without `--where`
- `delete` warns about dangling wiki-links before proceeding
- `delete` moves to `.trash/` by default (with collision-safe naming)
- `rename` auto-updates all `[[wiki-links]]` across the vault
- Writer detects and refuses to modify flow-style YAML (`[a, b]`) or multiline scalars (`|`, `>`)
- Files without frontmatter are loaded with empty fields (queryable by virtual fields, never silently skipped)

## Library usage (vaultdb-core)

Add to your `Cargo.toml`:

```toml
[dependencies]
vaultdb-core = { git = "https://github.com/rusenbb/vaultdb" }
```

The full public surface lives at the crate root: `Vault`, `Record`, `Value`, `Query`, `Expr`, `Predicate`, `LinkPredicate`, `LinkGraph`, `GraphScope`, `Direction`, `UpdateBuilder`, `DeleteBuilder`, `MoveBuilder`, `RenameBuilder`, `MutationReport`, `LoadResult`, `ParseError`, `VaultdbError`. All public data types are `Serialize`/`Deserialize`-able.

```rust
use vaultdb_core::{
    Expr, LinkPredicate, Query, UpdateBuilder, Value, Vault,
};

let vault = Vault::discover(std::path::Path::new("/path/to/vault"))?;

// Records that link to anything tagged topic/ai
let q = Query {
    folder: "notes".into(),
    filter: Some(Expr::LinksTo(LinkPredicate::Where(Box::new(
        Expr::parse("tags contains topic/ai")?,
    )))),
    select: None,
    sort: None,
    limit: None,
    recursive: false,
};
let hits = vault.query(&q)?;

// Plan-only mutation: see what would change without writing
let filter = Expr::parse("status = draft")?;
let plan = UpdateBuilder::new("notes", filter)
    .set("status", Value::String("published".into()))
    .plan(&vault)?;
for change in &plan.changes {
    println!("{}: {}", change.path.display(), change.description);
}
```

Every mutation builder exposes a `plan(&vault)` and an `execute(self, &vault)`. `plan` is read-only; `execute` runs the same computation and writes the result. The CLI's `--dry-run` flag is just `plan() + render`.

## MCP server (vaultdb-mcp)

`vaultdb-mcp` exposes the library as a Model Context Protocol server over stdio, so Claude, Cursor, and other MCP-aware clients can query and reason about a vault.

```bash
cargo install --path crates/vaultdb-mcp
```

Wire it into Claude Desktop's config (`~/.config/claude/claude_desktop_config.json` on Linux, `~/Library/Application Support/Claude/claude_desktop_config.json` on macOS):

```json
{
  "mcpServers": {
    "vaultdb": {
      "command": "vaultdb-mcp",
      "args": ["--vault", "/absolute/path/to/your/vault"]
    }
  }
}
```

Tools exposed: `query`, `find_by_name`, `list_folders`, `links`, `traverse`, `unresolved`, `schema_show`, `schema_infer`, plus four **plan-only** mutation tools (`plan_update`, `plan_delete`, `plan_move`, `plan_rename`) that show what a change would do without writing — agents propose, you (or the host) decide whether to apply.

There are intentionally no `execute_*` tools. Mutations go through the CLI or your own application code, with you in the loop.

## Claude Code integration

vaultdb ships with a [Claude Code](https://claude.ai/code) skill so LLM agents can use it directly. To install:

```bash
# Copy the skill to your personal skills directory
mkdir -p ~/.claude/skills/vaultdb
cp skills/vaultdb/SKILL.md ~/.claude/skills/vaultdb/SKILL.md
```

Then in any Claude Code session, the agent can invoke `/vaultdb` or use it automatically when you ask about your vault.

## Not Obsidian-specific

Despite being designed for Obsidian vaults, vaultdb works with any folder of `.md` files with YAML frontmatter. Hugo, Jekyll, Astro, Zola, or any static site generator's content directory is a valid target.

## License

MIT
