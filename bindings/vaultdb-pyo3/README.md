# vaultdb (Python)

Python bindings for [vaultdb-core](https://crates.io/crates/vaultdb-core).
Treat your Obsidian-style markdown vault as a queryable database
from a Jupyter notebook, a small script, or a data pipeline.

```python
import vaultdb

# 1. Point at a vault
root = vaultdb.open_vault("/Users/me/notes")

# 2. List records in a folder
notes = vaultdb.list_records(root, "research", recursive=True)
for n in notes:
    print(n["path"], n["fields"].get("tags"))

# 3. Run a where-DSL query
recent = vaultdb.query(
    root,
    folder="research",
    where_clause='date > "2025-01-01" and "draft" in tags',
    sort="date desc",
    limit=20,
)

# 4. Walk the wiki-link graph
graph = vaultdb.link_graph(root)
print(graph["incoming"]["Stanford University"])  # who links to Stanford
```

## Installation

```bash
pip install vaultdb
```

Wheels are published for CPython 3.9+ (stable ABI), Linux x86_64 +
aarch64, macOS x86_64 + arm64, and Windows x86_64.

## What's exposed

| Function          | Mirrors                                |
|-------------------|----------------------------------------|
| `open_vault`      | `Vault::with_root`                     |
| `list_records`    | `Vault::load_records`                  |
| `query`           | `Vault::query` (where-DSL + sort + limit) |
| `link_graph`      | `Vault::link_graph(GraphScope::All)`   |

## What's not (yet)

- Mutation builders (`UpdateBuilder`, `DeleteBuilder`,
  `RenameBuilder`, `MoveBuilder`) — Python's keyword-arg ergonomics
  don't translate the builder pattern cleanly. Read first; mutate
  via the Rust API or the [`vaultdb` CLI](https://crates.io/crates/vaultdb).
- Streaming `query_iter` — eager `query` is a closer fit for
  Python's idioms; streaming would require a custom iterator class
  that callers don't actually want.

Both are additive changes if a real use-case shows up.

## Building from source

```bash
pip install maturin
cd bindings/vaultdb-pyo3
maturin develop --release   # build + install in current venv
maturin build --release     # build a wheel under target/wheels/
```

## License

MIT — same as `vaultdb-core`.
