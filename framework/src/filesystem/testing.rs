//! Test-only helpers for the storage facade.
//!
//! `Storage::fake()` returns a `StorageFakeGuard` that:
//!
//! 1. Acquires a process-global `Mutex` so concurrent `#[tokio::test]` cases
//!    do not interleave registrations on the shared registry, and
//! 2. Resets the registry on construction and on drop, leaving the suite in a
//!    clean state for whichever test runs next.
//!
//! The mutex is intentionally held for the entire test body — this serializes
//! storage tests against each other, which is the price for a single global
//! disk registry. Other test categories are unaffected.

#![cfg(any(test, feature = "testing"))]

use std::sync::{Mutex, MutexGuard};

static FAKE_LOCK: Mutex<()> = Mutex::new(());

/// Guard returned by [`Storage::fake`](super::Storage::fake).
///
/// Holds the global fake-lock for the lifetime of the test and resets the
/// disk registry on drop so the next test starts from a clean slate.
pub struct StorageFakeGuard {
    _lock: MutexGuard<'static, ()>,
}

impl Drop for StorageFakeGuard {
    fn drop(&mut self) {
        super::registry::reset();
    }
}

/// Install the fake registry. Resets state, registers a `"default"` memory
/// disk so simple tests can call `Storage::disk("default")` without further
/// setup, and returns a guard whose `Drop` wipes the registry again.
pub(crate) fn install_fake() -> StorageFakeGuard {
    let lock = FAKE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    super::registry::reset();
    super::Storage::register_memory("default");
    StorageFakeGuard { _lock: lock }
}
