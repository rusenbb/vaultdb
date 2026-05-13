# Releasing vaultdb to crates.io

The standard path is **automated via GitHub Actions** — push a
`v*` tag, the publish workflow runs unattended.

## Automated path (recommended)

Pre-flight, on every push and PR, `.github/workflows/ci.yml`
runs `cargo test`, `cargo clippy -D warnings`, `cargo fmt --check`,
and a `cargo publish --dry-run` for `vaultdb-core` plus
`cargo package` for the other two. Manifest regressions surface
before tag time.

Release-time, push a `v*` tag and
`.github/workflows/publish.yml` takes over:

1. Publish `vaultdb-core` to crates.io.
2. Wait 60 seconds for the sparse index to settle.
3. Publish `vaultdb-mcp` and `vaultdb` (CLI) in parallel.
4. Open a draft GitHub Release with auto-generated notes.

```bash
# Bump the workspace version in Cargo.toml first, then:
git tag -a v1.1.1 -m "vaultdb v1.1.1"
git push origin v1.1.1
```

Tagging IS the approval. **Don't push tags from branches you
don't intend to release** — the workflow doesn't pause for human
review before publishing. If you tag the wrong commit, your only
recourse is `cargo yank` after the fact (which marks a version as
undesirable but doesn't reclaim the version number).

GitHub secrets needed (one-time setup):
- `CARGO_REGISTRY_TOKEN` — from
  <https://crates.io/me/account/tokens>. Scope it to "publish new
  versions of crates I own."

## Manual path (fallback)

If you need to publish without the workflow (e.g. a CI outage,
or a hotfix from a private fork), the dependency order is:

1. `vaultdb-core` → unblocks everything else
2. `vaultdb-mcp` and `vaultdb` (in any order, both depend only on
   vaultdb-core)
3. `eduport-core` (in the eduport repo — different working tree,
   different workflow)

```bash
# Pre-flight
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings

# 1. Publish vaultdb-core. Wait ~60s for the sparse index to
#    settle before the downstream crates resolve it.
cargo publish -p vaultdb-core
sleep 60

# 2. Publish the binaries.
cargo publish -p vaultdb-mcp
cargo publish -p vaultdb

# 3. Push the tag.
git push origin v1.0.0

# 4. (eduport repo) Trigger the eduport-core publish workflow
#    by pushing an `eduport-core-v*` tag. eduport's desktop app
#    uses a separate `desktop-v*` tag namespace.
cd /path/to/eduport
git tag -a eduport-core-v0.1.1 -m "eduport-core v0.1.1"
git push origin eduport-core-v0.1.1
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
