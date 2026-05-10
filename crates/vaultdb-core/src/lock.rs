//! Vault-scoped exclusive lock used to serialize mutations across processes.
//!
//! [`with_vault_lock`] acquires a `flock`-style exclusive lock on a hidden
//! sentinel file at `<vault>/.vaultdb/lock` for the duration of `op`. Two
//! `vaultdb-core` consumers calling `UpdateBuilder::execute` (or any other
//! mutation builder's `execute`) on the same vault path will serialize.
//!
//! ## What this protects against
//!
//! - Two `vaultdb-core` instances racing on the same file (e.g. the CLI and
//!   eduport-tauri running simultaneously, both writing the same record).
//! - Two threads in the same process racing on the same file.
//!
//! ## What this does NOT protect against
//!
//! - **External editors that don't take this lock** (Obsidian, Vim, VS Code).
//!   `flock` is advisory on Unix and only effective between processes that
//!   call it. The mitigation for editor races is mtime-based optimistic
//!   concurrency at write time, plus the atomic tempfile+rename in
//!   [`crate::writer::atomic_write`] which guarantees no partial-content
//!   reads. Eduport-core is expected to debounce its watcher long enough
//!   for atomic writes to settle.
//! - **Power loss between write and fsync.** Use the [`crate::WriteOptions`]
//!   `fsync` flag for durability.
//!
//! ## Layout
//!
//! ```text
//! <vault>/.vaultdb/
//!   lock              # this module's sentinel
//!   rename-journal/   # transactional rename journals (next phase)
//! ```
//!
//! `.vaultdb/` is hidden (dotfile) so it doesn't pollute regular `ls` output
//! in the user's vault.

use std::fs::OpenOptions;
use std::path::Path;

use fs2::FileExt;

use crate::error::VaultdbError;

/// Subdirectory under the vault root that holds vaultdb-core's
/// metadata (locks, journals).
pub(crate) const META_DIR: &str = ".vaultdb";

/// Filename of the vault-scoped mutation lock sentinel.
const LOCK_FILE: &str = "lock";

/// Run `op` while holding an exclusive advisory lock on `<vault>/.vaultdb/lock`.
///
/// The directory and lock file are created on first call and persist after.
/// The lock is released automatically when this function returns (success
/// or error) via the returned `File`'s `Drop` impl, even if `op` panics.
///
/// `op`'s error type must be convertible from `std::io::Error` so file-system
/// errors during lock acquisition can flow through naturally.
pub fn with_vault_lock<F, R, E>(vault_root: &Path, op: F) -> Result<R, E>
where
    F: FnOnce() -> Result<R, E>,
    E: From<std::io::Error>,
{
    let lock_dir = vault_root.join(META_DIR);
    std::fs::create_dir_all(&lock_dir).map_err(E::from)?;

    let lock_path = lock_dir.join(LOCK_FILE);
    let lock_file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(E::from)?;

    // `lock_exclusive` blocks until the lock is acquired. If another
    // vaultdb instance is mid-mutation, we wait for it. There's no
    // try-lock variant exposed here on purpose: callers shouldn't have
    // to choose between "wait" and "fail-fast" for the common case.
    lock_file.lock_exclusive().map_err(E::from)?;

    // Run the caller's operation. Drop of `lock_file` at end of scope
    // releases the flock; we don't need to do it explicitly. The result
    // is returned regardless of success/error so the caller's error
    // stays intact.
    let result = op();

    // Best-effort explicit unlock for clarity; Drop handles it too.
    let _ = FileExt::unlock(&lock_file);
    result
}

/// Convenience: same as [`with_vault_lock`] but pinned to `VaultdbError`.
pub(crate) fn with_lock<F, R>(vault_root: &Path, op: F) -> Result<R, VaultdbError>
where
    F: FnOnce() -> Result<R, VaultdbError>,
{
    with_vault_lock(vault_root, op)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::thread;
    use std::time::Duration;
    use tempfile::TempDir;

    fn make_vault() -> TempDir {
        let dir = TempDir::new().unwrap();
        std::fs::create_dir(dir.path().join(".obsidian")).unwrap();
        dir
    }

    #[test]
    fn lock_file_and_meta_dir_are_created() {
        let dir = make_vault();
        let result: Result<(), VaultdbError> = with_lock(dir.path(), || Ok(()));
        result.unwrap();

        assert!(dir.path().join(".vaultdb").is_dir());
        assert!(dir.path().join(".vaultdb").join("lock").is_file());
    }

    #[test]
    fn lock_serializes_concurrent_callers() {
        // Spawn N threads that each try to take the lock and increment a
        // counter, holding the lock briefly. If the lock works, the
        // observed counter values inside the critical section should
        // never collide (each thread sees a unique pre-increment value).
        let dir = make_vault();
        let vault_path = dir.path().to_path_buf();
        let counter = Arc::new(AtomicUsize::new(0));
        let collisions = Arc::new(AtomicUsize::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let vault_path = vault_path.clone();
            let counter = Arc::clone(&counter);
            let collisions = Arc::clone(&collisions);
            handles.push(thread::spawn(move || {
                let result: Result<(), VaultdbError> = with_lock(&vault_path, || {
                    let before = counter.load(Ordering::SeqCst);
                    // Hold the lock long enough that a buggy lock would
                    // visibly let another thread interleave.
                    thread::sleep(Duration::from_millis(20));
                    let after = counter.load(Ordering::SeqCst);
                    if before != after {
                        collisions.fetch_add(1, Ordering::SeqCst);
                    }
                    counter.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                });
                result.unwrap();
            }));
        }
        for h in handles {
            h.join().unwrap();
        }

        assert_eq!(
            counter.load(Ordering::SeqCst),
            8,
            "every thread should have incremented exactly once"
        );
        assert_eq!(
            collisions.load(Ordering::SeqCst),
            0,
            "lock failed to serialize: counter changed during a critical section"
        );
    }

    #[test]
    fn op_error_propagates_and_lock_releases() {
        let dir = make_vault();

        // First call returns an error from the closure. Subsequent calls
        // must still acquire the lock cleanly (i.e., the error path must
        // still release the lock).
        let result: Result<(), VaultdbError> = with_lock(dir.path(), || {
            Err(VaultdbError::SchemaError("intentional".into()))
        });
        assert!(matches!(
            result,
            Err(VaultdbError::SchemaError(ref m)) if m == "intentional"
        ));

        let result: Result<(), VaultdbError> = with_lock(dir.path(), || Ok(()));
        result.unwrap();
    }
}
