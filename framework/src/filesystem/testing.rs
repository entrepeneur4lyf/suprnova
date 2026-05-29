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
//!
//! # Disk assertions
//!
//! The [`DiskAssertExt`] trait blanket-implements four assertion helpers on
//! `opendal::Operator` so tests can write `disk.assert_exists("path").await`
//! the same way Laravel's `Storage::disk('local')->assertExists(...)` reads.

#![cfg(any(test, feature = "testing"))]

use crate::FrameworkError;
use crate::filesystem::DiskExt;
use opendal::Operator;
use std::future::Future;
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

/// Laravel-shape assertion helpers on top of [`opendal::Operator`].
///
/// Mirrors `Illuminate\Filesystem\FilesystemAdapter::assertExists` /
/// `assertMissing` / `assertCount` / `assertDirectoryEmpty`. All four panic
/// on failure with a message that names the disk path so test output points
/// at the broken assertion.
///
/// Gated on `#[cfg(any(test, feature = "testing"))]` so production code
/// cannot reach for them by accident.
pub trait DiskAssertExt {
    /// Assert that the path exists. Optionally also assert that the file
    /// contents match `expected`. Panics on mismatch.
    fn assert_exists<'a>(&'a self, path: &'a str) -> impl Future<Output = ()> + Send + 'a;

    /// Assert that the file at `path` contains exactly `expected` bytes.
    /// Panics on mismatch.
    fn assert_contents<'a>(
        &'a self,
        path: &'a str,
        expected: &'a [u8],
    ) -> impl Future<Output = ()> + Send + 'a;

    /// Assert that the path does not exist. Panics if it does.
    fn assert_missing<'a>(&'a self, path: &'a str) -> impl Future<Output = ()> + Send + 'a;

    /// Assert that `directory` contains exactly `expected` entries (files
    /// plus subdirectories). With `recursive = true`, counts every
    /// descendant. Panics on mismatch.
    fn assert_count<'a>(
        &'a self,
        directory: &'a str,
        expected: usize,
        recursive: bool,
    ) -> impl Future<Output = ()> + Send + 'a;

    /// Assert that `directory` has no entries (no files, no subdirectories).
    /// Recursive — every descendant counts. Panics if any entry exists.
    fn assert_directory_empty<'a>(
        &'a self,
        directory: &'a str,
    ) -> impl Future<Output = ()> + Send + 'a;
}

impl DiskAssertExt for Operator {
    async fn assert_exists<'a>(&'a self, path: &'a str) {
        let present = self
            .exists(path)
            .await
            .unwrap_or_else(|e| panic!("storage exists({path}) errored: {e}"));
        assert!(present, "expected disk path to exist: {path}");
    }

    async fn assert_contents<'a>(&'a self, path: &'a str, expected: &'a [u8]) {
        self.assert_exists(path).await;
        let actual = self
            .get(path)
            .await
            .unwrap_or_else(|e| panic!("storage get({path}) errored: {e}"));
        assert_eq!(
            actual, expected,
            "disk path {path} contents do not match expected"
        );
    }

    async fn assert_missing<'a>(&'a self, path: &'a str) {
        let present = self
            .exists(path)
            .await
            .unwrap_or_else(|e| panic!("storage exists({path}) errored: {e}"));
        assert!(!present, "expected disk path to be missing: {path}");
    }

    async fn assert_count<'a>(&'a self, directory: &'a str, expected: usize, recursive: bool) {
        let actual = count_entries(self, directory, recursive)
            .await
            .unwrap_or_else(|e| panic!("storage list({directory}) errored: {e}"));
        assert_eq!(
            actual, expected,
            "expected directory {directory} to contain {expected} entries, found {actual}"
        );
    }

    async fn assert_directory_empty<'a>(&'a self, directory: &'a str) {
        let actual = count_entries(self, directory, true)
            .await
            .unwrap_or_else(|e| panic!("storage list({directory}) errored: {e}"));
        assert_eq!(
            actual, 0,
            "expected directory {directory} to be empty, found {actual} entries"
        );
    }
}

/// Count entries under `directory` (excluding the directory itself), honouring
/// `recursive` to descend into subdirectories. Used by [`DiskAssertExt`].
async fn count_entries(
    op: &Operator,
    directory: &str,
    recursive: bool,
) -> Result<usize, FrameworkError> {
    let files = op.files(directory, recursive).await?;
    let dirs = op.directories(directory, recursive).await?;
    Ok(files.len() + dirs.len())
}
