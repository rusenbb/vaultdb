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

`bindings/vaultdb-pyo3/` and `bindings/vaultdb-wasm/` are
in-workspace crates that ship to non-crates.io registries:

- **`vaultdb-pyo3`** → Python wheel via `maturin`, published to
  PyPI as `vaultdb`.
- **`vaultdb-wasm`** → wasm-pack package, published to npm as
  `@rusenbb/vaultdb`.

### PyPI publish (vaultdb-pyo3 → `vaultdb`)

```bash
# Prereqs: pipx install maturin (or pip install --user maturin),
# plus a PyPI API token at ~/.pypirc or via MATURIN_PYPI_TOKEN.

cd bindings/vaultdb-pyo3

# Build a stable-ABI wheel (one wheel covers CPython 3.9+).
maturin build --release --strip

# Build sdist too so pip can fall back when no wheel matches.
maturin sdist

# Publish both. `--skip-existing` is harmless and lets you re-run
# the command after a partial failure.
maturin publish --skip-existing
```

For multi-platform wheels (Linux x86_64 + aarch64, macOS
x86_64 + arm64, Windows x86_64), run `maturin build` inside each
target's CI runner. The release workflow under `.github/workflows`
in eduport's repo serves as a reference.

### npm publish (vaultdb-wasm → `@rusenbb/vaultdb`)

```bash
# Prereqs: cargo install wasm-pack, plus `npm login` (the npm
# token must have publish access to the @rusenbb scope).

cd bindings/vaultdb-wasm
wasm-pack build --target bundler --release --scope rusenbb

# wasm-pack writes a publish-ready package under pkg/. Inspect it
# before publishing — particularly the package.json's `name`
# field (should be `@rusenbb/vaultdb`).
cat pkg/package.json | jq .name

# Publish to npm.
cd pkg
npm publish --access public
```

### Verifying after publish

- <https://pypi.org/project/vaultdb/>
- <https://www.npmjs.com/package/@rusenbb/vaultdb>

Each page should show v1.0.0 with the README rendered. A trial
install in a clean venv / npm scratch project is a good final
sanity check.
