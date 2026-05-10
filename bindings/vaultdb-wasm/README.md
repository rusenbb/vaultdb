# @rusenbb/vaultdb (WASM)

Browser/Node.js bindings for [vaultdb-core](https://crates.io/crates/vaultdb-core).
Parse markdown frontmatter, extract `[[wikilinks]]`, and run
where-DSL queries against a single record — all from JavaScript,
no native binary needed.

```js
import { parseRecord, evaluateWhere, extractLinks, version }
    from '@rusenbb/vaultdb';

const note = `---
title: Stanford
tags: [reach]
date: 2026-05-09
---
Notes on visiting [[Stanford University]].
`;

const record = parseRecord("notes/stanford.md", note);
console.log(record.fields.tags);            // ["reach"]

const matches = evaluateWhere(
    record,
    'date > "2025-01-01" and "reach" in tags'
);
console.log(matches);                       // true

const links = extractLinks(record.body);
console.log(links);                         // ["Stanford University"]

console.log(`vaultdb-wasm ${version()}`);
```

## Why this binding doesn't include `Vault`

`Vault` walks the filesystem, holds an `fs2`-based vault lock,
and runs a crash-recovery rename journal. None of those work on
`wasm32-unknown-unknown` — the browser sandbox simply doesn't
expose them.

If you need persistent vault-like behaviour in the browser, build
it on top of IndexedDB (or OPFS) and feed individual files
through `parseRecord` / `evaluateWhere`. A future
`BrowserVault` package may layer that on; for now the binding
stays at the parser level so its API is small, predictable, and
won't fail at runtime.

## Installation

```bash
npm install @rusenbb/vaultdb
```

## Building from source

```bash
cargo install wasm-pack
cd bindings/vaultdb-wasm
wasm-pack build --target bundler --release
# Output lands in pkg/ — that's what gets published to npm.
```

## License

MIT — same as `vaultdb-core`.
