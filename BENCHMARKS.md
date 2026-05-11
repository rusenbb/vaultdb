# Benchmarks

These are real numbers from a real run, not estimates. The harness
that produced them is checked in at
[`crates/vaultdb-core/examples/bench.rs`](crates/vaultdb-core/examples/bench.rs)
— you can reproduce them on your own machine in about ten seconds.

> **Why these numbers exist:** vaultdb's defining design choice is
> "no daemon, no cache, no state files — every read traverses the
> filesystem fresh." That sounds nice in prose, but the only honest
> way to defend the choice is to publish how it actually behaves
> at scale. These numbers are how we keep ourselves honest.

## Methodology

- **Harness:** `cargo run --release --example bench -- <N>` against
  a synthetic vault of `N` markdown notes. Each note has a short
  YAML frontmatter (`status`, two tags) and a body with three
  `[[wikilinks]]` to other notes (some resolving, some dangling —
  closer to a real vault than a perfectly-formed one).
- **Per-measurement:** three runs after one warm-up read, lowest
  reported. Warm-up loads the directory into page cache so we're
  measuring vaultdb, not the cold cache.
- **Workloads:**
  - **Frontmatter query** — `status = active`. Pure frontmatter
    scan, no link graph. The hot path for filter-on-property.
  - **Graph query** — `_link_count > 0`. Forces the link graph to
    be built, then filtered. The expensive shape.
  - **`link_graph(All)`** — just the graph build. Reported on its
    own so you can subtract it from the graph-query number.
  - **Streaming top-K** — `query_iter` with `sort=_name desc` and
    `limit=10`. Validates the bounded-heap optimization (sort
    should not pay full O(N log N)).
  - **Streaming pure** — `query_iter` with no sort, no limit.
    The O(1) RAM happy path.

## Host

These numbers come from a Linux desktop. Numbers will vary on
other hardware; the *shape* (roughly linear scaling, sub-second
through ~50k notes) should hold.

- CPU: Intel Core i7-14700K
- RAM: 32 GB
- OS: Ubuntu 24.04 (kernel 6.17.0)
- Storage: NVMe SSD
- Rust: 1.95.0, `--release`
- vaultdb-core: v1.0.0

## Results

| Workload                    |       1 000 notes |      10 000 notes |     100 000 notes |
|-----------------------------|------------------:|------------------:|------------------:|
| Vault generation (write)    |          ~10 ms   |          ~90 ms   |          ~880 ms  |
| Frontmatter query           |          **5 ms** |         **59 ms** |        **651 ms** |
| Graph query (builds graph)  |          **7 ms** |         **88 ms** |       **1 032 ms** |
| `link_graph(All)` (only)    |            6 ms   |           70 ms   |          819 ms   |
| Streaming top-K (sort+10)   |            5 ms   |           64 ms   |          733 ms   |
| Streaming pure (no sort)    |            5 ms   |           57 ms   |          637 ms   |

### How to read this table

- **Scaling is roughly linear in vault size.** Going 10× the notes
  costs about 10–12× the time. There is no superlinear cliff up
  through 100k notes.
- **The link graph costs about as much as a frontmatter scan.**
  Building the graph is one extra YAML+body parse per file plus
  a hashmap build; on this hardware, that doubles the work.
- **Streaming is competitive with eager** at every scale, and
  the top-K shape doesn't pay the full sort cost — the bounded
  heap optimization is doing its job.
- **At 1 000 notes**, every operation is comfortably under 10 ms.
  This is the *desktop personal vault* regime, and vaultdb is
  effectively instant.
- **At 10 000 notes**, queries land in 60–90 ms — well under the
  100-ms perceptual threshold for "feels instant."
- **At 100 000 notes**, the largest realistic personal vault,
  every operation completes in **under 1.1 seconds.** For
  interactive desktop use that's the upper edge of acceptable;
  for batch or server use it's fine.

## Memory

Streaming queries (`query_iter`) hold one parsed `Record` plus the
top-K heap in memory. At 100k notes this is on the order of a few
hundred KB, not megabytes. The eager `query` path materializes the
whole result set; budget for record size × hit count if you use
it on a big vault.

## What's not here yet

- **Cold-cache numbers.** All measurements above are warm-cache
  (the page cache is loaded). Real first-open latency on a cold
  vault will be higher — somewhere between 1.5× and 3× depending
  on storage. We'll publish cold numbers separately.
- **Mutation throughput.** Every mutation is one atomic-rename
  per affected file. We have not yet measured throughput; the
  bottleneck is fsync latency, not vaultdb code, so the number
  is more about your filesystem than about us.
- **Comparative benchmarks.** "How does this compare to
  Obsidian's Dataview / Logseq's queries / grep + jq?" — fair
  questions, separate exercise.

## Regenerate locally

```bash
cargo run --release --example bench -- 1000
cargo run --release --example bench -- 10000
cargo run --release --example bench -- 100000
```

The harness cleans up after itself. Notes are written under
`$TMPDIR/vaultdb-bench-<N>/` and deleted at the end.

## When to update this file

- After any change to `vault.rs`, `query.rs`, `links.rs`, or
  `record.rs` that could plausibly affect performance.
- On every minor version bump.
- If you ever see > 20% regression in a single workload,
  treat it as a bug, not a fact-of-life.
