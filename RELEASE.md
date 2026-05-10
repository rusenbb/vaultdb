# Releasing vaultdb to crates.io

This runbook publishes the three workspace crates and the language
bindings shipped under `bindings/`. Run it by hand — every step is
externally visible, and crates.io / PyPI / npm version slots are
permanent.

## Prerequisites

- `cargo login <token>` once per dev machine. Get the token from
  <https://crates.io/me/account/tokens>.
- A clean working tree (`git status` shows nothing).
- The release tag (`v1.0.0`) already exists locally.

## Order matters

`vaultdb-mcp`, `vaultdb` (CLI), and the eduport workspace's
`eduport-core` all depend on `vaultdb-core`. crates.io can't index
dependencies that don't exist yet, so publish in dependency order:

1. `vaultdb-core` → unblocks everything else
2. `vaultdb-mcp` and `vaultdb` (in any order, both depend only on
   vaultdb-core)
3. `eduport-core` (in the eduport repo — different working tree)

## The commands

```bash
# From the vaultdb workspace root
cd /path/to/vaultdb

# 1. Verify everything's clean
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# 2. Publish vaultdb-core. Wait ~30s for the index to settle
#    before the next step (cargo will refuse to publish a crate
#    whose dep can't be resolved on the index yet).
cargo publish -p vaultdb-core
sleep 30

# 3. Publish vaultdb-mcp and vaultdb (CLI). They resolve
#    vaultdb-core from the now-populated index.
cargo publish -p vaultdb-mcp
cargo publish -p vaultdb

# 4. Push the tag so GitHub releases can pick it up
git push origin v1.0.0

# 5. (eduport repo) Publish eduport-core
cd /path/to/eduport
cargo publish -p eduport-core
```

## Verifying after publish

- <https://crates.io/crates/vaultdb-core>
- <https://crates.io/crates/vaultdb-mcp>
- <https://crates.io/crates/vaultdb>
- <https://crates.io/crates/eduport-core>

Each page should show v1.0.0 and a populated README.

## Yanking a bad release

If a release has a real bug:

```bash
cargo yank --version 1.0.0 vaultdb-core
```

Yanked versions are still downloadable for existing lockfiles but
won't be picked up by `cargo update`. The version slot is gone
forever — the next release must increment.

## Language bindings (Phase E)

`bindings/vaultdb-pyo3/` and `bindings/vaultdb-wasm/` ship as
separate crates under the same workspace. They are NOT published
to crates.io; they have their own publishing pipelines:

- **`vaultdb-pyo3`** → built into a Python wheel via `maturin`,
  published to PyPI as `vaultdb` (PyPI package name; the crate
  name is `vaultdb-pyo3` to disambiguate at the Rust level).
- **`vaultdb-wasm`** → built via `wasm-pack` into a JS package,
  published to npm as `@rusenbb/vaultdb` (the leading `@` is
  required because the bare `vaultdb` slot on npm is taken).

See each binding's `RELEASE.md` for the build + publish steps.
