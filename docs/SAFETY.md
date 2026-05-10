# vaultdb safety model

This document describes what `vaultdb-core` guarantees about your data
and what it does not. It is the definitive answer to the question
"is it safe to point this at my actual vault?"

The audience is engineers building on top of `vaultdb-core` (eduport,
third-party Rust apps, language-binding consumers via pyo3 / wasm).
End users of those apps should read whatever safety document the app
itself ships.

## Concurrency model

vaultdb-core uses a **vault-scoped exclusive write lock**. Every
mutation (`UpdateBuilder::execute`, `DeleteBuilder::execute`,
`MoveBuilder::execute`, `RenameBuilder::execute`) acquires a lock at
`<vault>/.vaultdb/lock` for the duration of its work. Concurrent
mutations from any number of `vaultdb-core` consumers serialise
cleanly — the second caller waits for the first to finish, then
re-reads any records it needs.

### What this protects against

- Two `vaultdb-core` instances racing on the same file (e.g. the CLI
  and an eduport-tauri process running simultaneously).
- Two threads in the same process racing on the same mutation builder.
- A long-running mutation holding the lock against drive-by concurrent
  ones; correctness is preserved at the cost of waiting.

### What this does NOT protect against

**External editors that don't take this lock.** `flock` (POSIX) and
`LockFileEx` (Windows) are advisory: only processes that explicitly
call them participate. Obsidian, Vim, VS Code, sync clients (Dropbox,
iCloud, Syncthing) do not. So:

- If the user is editing `Stanford.md` in Obsidian while eduport-tauri
  fires a mutation against the same file, the two writes can interleave.
  `vaultdb-core`'s atomic tempfile+rename write means readers never see
  partial content, but the rename can still clobber an Obsidian save
  that landed between vaultdb-core's read and write.
- Ship-level mitigation: detect via mtime check. (Not in v0.3.)
- App-level mitigation: eduport-core's watcher should debounce long
  enough for vaultdb-core's atomic writes to settle, and the UI should
  warn the user if external edits are detected during a vaultdb-driven
  mutation.

**Multi-machine sync.** Sync clients that detect both your local
write and a remote change can produce conflict files (`Stanford
(conflicted copy 2026-05-10).md`). vaultdb-core sees these as
ordinary records — no special handling. Apps that care should detect
them via `Vault::list_files` and surface them.

**Power loss without `fsync`.** See "Durability" below.

## Atomicity

Every individual file write goes through `writer::atomic_write_with`,
which writes to a tempfile in the same directory and then renames it
over the target. The rename is atomic on:

- POSIX same-filesystem rename (default).
- Windows `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`.

Concurrent readers either see the full old content or the full new
content. They never see a partial write or a zero-length file.

### Multi-file atomicity for renames

`RenameBuilder::execute` is the only mutation that touches multiple
files in one logical operation: the source file rename plus every
backlink rewrite. To make the *combined* operation crash-recoverable,
vaultdb-core writes a journal at
`<vault>/.vaultdb/rename-journal/<timestamp>.json` *before* any
disk-modifying step. On crash:

- Source file rename hadn't happened: replay does the rename, then
  the rewrites.
- Source file rename had happened, some rewrites incomplete: replay
  finishes the rewrites idempotently.
- Both source and dest are gone (user manually deleted): journal is
  treated as stale and removed.

Recovery happens automatically before each new mutation (the lock-
acquiring builder runs `journal::replay_all` first), and can also be
invoked explicitly via `Vault::recover()`. Long-lived consumers
(eduport-tauri, etc.) should call `Vault::recover()` at startup so
leftover work from a previous crash is finished before any new
mutation.

The other mutations (`UpdateBuilder`, `DeleteBuilder`, `MoveBuilder`)
are inherently retryable: each affects independent files, so re-
running the operation against the unchanged-yet records finishes the
work. They don't need a journal.

## Durability (`WriteOptions::fsync`)

The default mutation does NOT fsync. After a successful return from
`execute()`, the change is in the OS page cache and the rename's
directory entry is in the filesystem journal, but the file's data
pages may not yet be on stable storage. A power loss seconds later
can leave the rename visible while pointing at zeros or stale data.
This matches the behaviour of `std::fs::write` and is consistent with
how most non-database desktop apps treat normal file writes.

For durable mutations, opt in:

```rust
UpdateBuilder::new("notes", filter)
    .set("status", Value::String("published".into()))
    .fsync(true)            // alias for write_options(WriteOptions::durable())
    .execute(&vault)?;
```

With `fsync: true`, every modified file's data is fsynced before the
rename, and every modified directory's dirent is fsynced after the
rename. After `execute()` returns, the change survives sudden power
loss.

Cost: each fsync is one disk-flush IO. On consumer SSDs that's
typically 1–10ms; on spinning disks 10–50ms. For an update touching
N files with `fsync=true`, expect 2N IOs (data + parent dir per file).
The CLI defaults to `fsync=false` for speed; eduport-tauri's typed
Tauri commands should set `fsync=true` for user-initiated saves and
leave it off for high-frequency batch operations.

## Per-mutation safety summary

| Mutation         | Atomic per file | Multi-file atomicity | Crash-recoverable  | Durable with `fsync(true)` |
|------------------|-----------------|----------------------|--------------------|----------------------------|
| `UpdateBuilder`  | ✓ (tempfile+rename) | n/a (independent files) | inherently retryable | ✓                  |
| `DeleteBuilder`  | ✓ (rename to .trash, or unlink) | n/a (independent files) | inherently retryable | ✓ (parent dir fsync) |
| `MoveBuilder`    | ✓ (rename) | n/a (independent files) | inherently retryable | ✓ (both parents fsync) |
| `RenameBuilder`  | ✓ (rename + atomic_write per backlink) | ✓ (journal) | ✓ (journal replay) | ✓                  |

## Filesystem assumptions

vaultdb-core requires:

- A POSIX-or-NTFS filesystem with atomic rename within a single
  directory. Almost everything modern qualifies (ext4, btrfs, xfs,
  zfs, apfs, ntfs).
- Read-write access to the vault root, including the ability to
  create the hidden `.vaultdb/` subdirectory for locks and journals.
- Case-preserving filenames. (`Stanford.md` and `stanford.md` are
  different records on case-sensitive FS, the same on case-insensitive.
  vaultdb-core makes no attempt to normalise; behaviour matches the
  underlying filesystem.)

vaultdb-core does NOT require:

- A specific OS or kernel version.
- Any database server, daemon, or background process.
- Network-mounted vaults (NFS, SMB) work but with caveats — `flock`
  semantics over network filesystems are weaker than local. Avoid
  concurrent multi-machine writes against the same vault on NFS.

## Recommended startup sequence for long-lived consumers

```rust
let vault = Vault::discover(start_path)?;
let recovered = vault.recover()?;
if recovered > 0 {
    tracing::info!("Replayed {} pending journal(s) from previous run", recovered);
}
// ...resume normal operation
```

Eduport-tauri should run this once at boot, before wiring up its
watcher and FTS5 reconcile, so the vault is in a known-good state
before the rest of the app starts observing it.

## Reporting safety bugs

If you discover a way to corrupt a vault through the public API
(`Vault`, mutation builders, the journal module's `replay`/`recover`
functions), please open an issue. Include the minimal reproduction:
exact API calls, vault state before, observed state after.
