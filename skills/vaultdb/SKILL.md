---
name: vaultdb
description: Query, mutate, and traverse Obsidian vaults using vaultdb — a markdown database engine with citation graph support. Use when the user asks to query notes, find orphans, explore the knowledge graph, update frontmatter, or manage their vault.
allowed-tools: Bash(vaultdb *)
---

You have access to `vaultdb`, a CLI tool that treats folders of markdown files as a document database. YAML frontmatter fields are queryable columns, `[[wiki-links]]` form a citation graph.

**Always check that vaultdb is installed before using it.** Run `which vaultdb` first. If not installed: `cargo install vaultdb`.

## Data model

```
Folder             →  Table
.md file           →  Record
Frontmatter fields →  Columns
[[wiki-links]]     →  Graph edges
```

## Virtual fields (available on every record)

| Field | Type | Description |
|-------|------|-------------|
| `_name` | string | Filename without `.md` |
| `_path` | string | Relative path from vault root |
| `_folder` | string | Parent folder name |
| `_modified` | string | File modification time |
| `_created` | string | File creation time |
| `_links` | list | Outgoing wiki-link targets |
| `_link_count` | integer | Number of outgoing links |
| `_backlinks` | list | Notes that link to this note |
| `_backlink_count` | integer | Number of incoming links |
| `_body_length` | integer | Character count of body (after frontmatter) |
| `_length` | integer | Total file character count |

## Vault auto-detection

vaultdb finds the vault root by walking up from cwd looking for `.obsidian/`. You can also pass `--vault <path>` explicitly. Always use `--vault` if the user's cwd is not inside the vault.

## Commands reference

### Read operations

```bash
# Query with filtering, sorting, output formats
vaultdb query <folder> [--where EXPR...] [--select F1,F2] [--sort FIELD] [--desc] [--limit N] [--format table|json|csv|yaml]

# Count matching records
vaultdb count <folder> [--where EXPR...]

# List all frontmatter fields with types and frequencies
vaultdb fields <folder>

# List all tags with usage counts
vaultdb tags <folder>
```

### Where expression syntax

```
FIELD = VALUE            # exact match
FIELD != VALUE           # not equal
FIELD > VALUE            # comparison (numeric if both sides parse as numbers)
FIELD < VALUE
FIELD >= VALUE
FIELD <= VALUE
FIELD contains VALUE     # list membership or substring
FIELD !contains VALUE    # negated
FIELD startswith VALUE
FIELD !startswith VALUE
FIELD endswith VALUE
FIELD matches REGEX      # regex
FIELD !matches REGEX
FIELD exists             # present and non-null
FIELD !exists            # absent or null
FIELD missing            # same as !exists
```

**AND**: multiple `--where` flags are AND-ed.
**OR**: use `||` within a single `--where`: `--where "status = active || status = pending"`
**NOT**: prefix operator with `!`: `!contains`, `!startswith`, `!exists`, `!matches`

### Graph operations

```bash
# Show outgoing + incoming links for a note
vaultdb links <note_name> [--folder <folder>] [--direction outgoing|incoming|both]

# BFS traversal from a starting note
vaultdb traverse <note_name> [--folder <folder>] [--depth N] [--direction outgoing|incoming|both] [--where EXPR...] [--select FIELDS]

# Find [[wiki-links]] to non-existent files
vaultdb unresolved [folder] [--from <note_name> --depth N] [-v]
```

### Relational joins (on the query command)

```bash
# Notes that link to a specific note
--links-to <note_name>

# Notes linked from a specific note
--linked-from <note_name>

# Notes that link to ANY note matching a condition
--links-to-where "EXPR"

# Notes linked from ANY note matching a condition
--linked-from-where "EXPR"
```

These compose with `--where`, `--sort`, `--select`, etc.

### Write operations

**All mutations support `--dry-run` to preview changes. Always use `--dry-run` first when the user hasn't explicitly asked for immediate execution.**

```bash
# Create a new note (optionally from a template)
vaultdb create <folder> --name <name> [--template <path>] [--set FIELD=VALUE...]

# Update frontmatter fields
vaultdb update <folder> --where EXPR... --set FIELD=VALUE
vaultdb update <folder> --where EXPR... --unset FIELD
vaultdb update <folder> --where EXPR... --add-tag TAG
vaultdb update <folder> --where EXPR... --remove-tag TAG

# Rename with auto wiki-link updates across vault
vaultdb rename <old_name> <new_name> [--folder <folder>]

# Move files between folders
vaultdb move <folder> --where EXPR... --to <target_folder>

# Delete (moves to .trash/ by default)
vaultdb delete <folder> --where EXPR... [--force]
```

Mutations require at least one `--where` condition (safety).

### Schema

```bash
vaultdb schema init <folder>       # infer schema from existing data
vaultdb schema show <folder>       # display schema
vaultdb schema validate <folder>   # check records against schema
```

### Global flags

```
--vault <PATH>     vault root (default: auto-detect .obsidian/)
--recursive        include subfolders
--verbose / -v     verbose output
--dry-run          preview mutations without writing
```

## Usage guidelines

1. **Start with read operations** to understand the vault structure: `fields`, `tags`, `count`.
2. **Use `--dry-run` for all mutations** unless the user explicitly asks to execute.
3. **Use `--format json`** when you need to parse the output programmatically.
4. **Graph fields (`_link_count`, `_backlink_count`) require reading file content** — slightly slower than frontmatter-only queries on very large vaults, but negligible under 50K files.
5. **The `--where` flag requires spaces around operators**: `"field = value"` not `"field=value"`.
6. **Folder paths are relative to vault root**: `3-Notes`, `5-Tasks/archive`, not absolute paths.

## Example patterns

```bash
# Vault overview
vaultdb fields 3-Notes
vaultdb tags 3-Notes

# Find notes by topic
vaultdb query 3-Notes --where "tags contains topic/ai" --select "_name,_backlink_count" --sort _backlink_count --desc

# Knowledge graph hubs
vaultdb query 3-Notes --select "_name,_backlink_count,_link_count" --sort _backlink_count --desc --limit 20

# Orphan notes (nothing links in or out)
vaultdb query 3-Notes --where "_backlink_count = 0" --where "_link_count = 0"

# Stub notes (empty or very short body)
vaultdb query 3-Notes --where "_body_length < 50" --select "_name,_body_length" --sort _body_length

# Unresolved links (notes to write next)
vaultdb unresolved 3-Notes
vaultdb unresolved 3-Notes --from "Machine Learning" --depth 3 -v

# Cross-domain connections
vaultdb query 3-Notes --links-to-where "tags contains topic/ai" --where "tags contains type/concept" --select "_name,_backlink_count"

# Explore a concept's neighborhood
vaultdb traverse Microservices --depth 2 --where "tags contains type/concept"

# Bulk tag update (always dry-run first)
vaultdb update 3-Notes --where "director contains Chaplin" --add-tag "director/charlie-chaplin" --dry-run
```
