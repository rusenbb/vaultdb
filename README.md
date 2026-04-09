# vaultdb

A database engine for your markdown files. Query, filter, mutate, and traverse [Obsidian](https://obsidian.md) vaults (or any folder of `.md` files with YAML frontmatter) from the command line.

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

No daemon, no cache, no state files. Every command reads the current `.md` files directly. Edit in Obsidian, query with vaultdb — they coexist without conflict.

## Install

```bash
# From source
git clone https://github.com/rusenbb/vaultdb.git
cd vaultdb
cargo install --path .

# Or just build
cargo build --release
# Binary at target/release/vaultdb
```

Requires Rust 1.75+.

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
FIELD exists             # field is present and non-null
FIELD missing            # field is absent or null
FIELD !exists            # negated exists (same as missing)
```

Multiple `--where` flags are AND-ed. Use `||` within a single `--where` for OR:

```bash
--where "status = active || status = pending"
```

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

## Performance

No caching, no indexing — reads files fresh on every command.

| Scale | Frontmatter query | Graph query |
|-------|------------------|-------------|
| 700 files | 16ms | 17ms |
| 10K files | ~200ms | ~250ms |
| 50K files | ~1s | ~1.2s |

Two-parser architecture: `serde_yaml` for fast reads, line-by-line string manipulation for formatting-preserving writes.

## Safety

- `--dry-run` previews all mutations before writing
- `update`, `move`, `delete` refuse to run without `--where`
- `delete` warns about dangling wiki-links before proceeding
- `delete` moves to `.trash/` by default (with collision-safe naming)
- `rename` auto-updates all `[[wiki-links]]` across the vault
- Writer detects and refuses to modify flow-style YAML (`[a, b]`) or multiline scalars (`|`, `>`)
- Files without frontmatter are loaded with empty fields (queryable by virtual fields, never silently skipped)

## Not Obsidian-specific

Despite being designed for Obsidian vaults, vaultdb works with any folder of `.md` files with YAML frontmatter. Hugo, Jekyll, Astro, Zola, or any static site generator's content directory is a valid target.

## License

MIT
