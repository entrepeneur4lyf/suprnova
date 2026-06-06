//! Small helpers to handle poisoned locks consistently across the framework.
//!
//! Policy (2026-05):
//! - We treat a poisoned lock as an internal error.
//! - Callers should almost always get a `FrameworkError` instead of panicking.
//! - This prevents one bad request from taking down an entire subsystem.
//!
//! Each helper takes a `context` label naming the subsystem that owns the
//! lock (e.g. `"connection registry"`, `"payments registry"`, `"db event
//! listeners"`). The label is embedded in the resulting `FrameworkError`
//! message so dev-only `debug_message` payloads and operator logs can tell
//! which lock poisoned without forcing every caller to wrap the result with
//! its own context.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::error::FrameworkError;

/// Acquire a read guard on an `RwLock`, returning a `FrameworkError` on poison.
///
/// `context` identifies the lock's owning subsystem (e.g. `"connection
/// registry"`). It is woven into the error message so the poison source is
/// visible without the caller having to wrap the error.
pub(crate) fn read<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> Result<RwLockReadGuard<'a, T>, FrameworkError> {
    lock.read()
        .map_err(|_| FrameworkError::internal(format!("{context} lock poisoned")))
}

/// Acquire a write guard on an `RwLock`, returning a `FrameworkError` on poison.
///
/// `context` identifies the lock's owning subsystem (e.g. `"connection
/// registry"`). It is woven into the error message so the poison source is
/// visible without the caller having to wrap the error.
pub(crate) fn write<'a, T>(
    lock: &'a RwLock<T>,
    context: &'static str,
) -> Result<RwLockWriteGuard<'a, T>, FrameworkError> {
    lock.write()
        .map_err(|_| FrameworkError::internal(format!("{context} lock poisoned")))
}

/// Acquire a guard on a `Mutex`, returning a `FrameworkError` on poison.
///
/// `context` identifies the lock's owning subsystem. It is woven into the
/// error message so the poison source is visible without the caller having
/// to wrap the error.
pub(crate) fn lock<'a, T>(
    lock: &'a Mutex<T>,
    context: &'static str,
) -> Result<MutexGuard<'a, T>, FrameworkError> {
    lock.lock()
        .map_err(|_| FrameworkError::internal(format!("{context} lock poisoned")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use std::thread;

    /// Poison a fresh `RwLock` by panicking while holding its write guard.
    fn poison_rw<T: Send + Sync + 'static>(rw: &Arc<RwLock<T>>) {
        let clone = Arc::clone(rw);
        let _ = thread::spawn(move || {
            let _g = clone.write().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(rw.is_poisoned(), "test setup: RwLock must be poisoned");
    }

    /// Propagate policy: a poisoned lock surfaces as a `FrameworkError`
    /// rather than panicking the whole subsystem. Notifications and mail
    /// registries `?` these helpers.
    #[test]
    fn helpers_return_err_on_poison_instead_of_panicking() {
        let rw = Arc::new(RwLock::new(0u32));
        poison_rw(&rw);
        assert!(
            read(&rw, "test registry").is_err(),
            "lock::read must return Err on poison",
        );
        assert!(
            write(&rw, "test registry").is_err(),
            "lock::write must return Err on poison",
        );

        let mtx = Arc::new(Mutex::new(0u32));
        let mtx_clone = Arc::clone(&mtx);
        let _ = thread::spawn(move || {
            let _g = mtx_clone.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(mtx.is_poisoned(), "test setup: Mutex must be poisoned");
        assert!(
            lock(&mtx, "test mutex").is_err(),
            "lock::lock must return Err on poison",
        );
    }

    /// Each helper's `FrameworkError::internal` message includes the
    /// caller-supplied context so logs / dev `debug_message` payloads can
    /// tell `connection registry` poison from `payments registry` poison
    /// without the caller having to wrap the error.
    #[test]
    fn error_message_includes_context_label() {
        let rw = Arc::new(RwLock::new(0u32));
        poison_rw(&rw);

        let err = read(&rw, "connection registry").expect_err("expected poison Err");
        let msg = err.to_string();
        assert!(
            msg.contains("connection registry"),
            "read err must name subsystem, got: {msg}",
        );

        let err = write(&rw, "payments registry").expect_err("expected poison Err");
        let msg = err.to_string();
        assert!(
            msg.contains("payments registry"),
            "write err must name subsystem, got: {msg}",
        );

        let mtx = Arc::new(Mutex::new(0u32));
        let mtx_clone = Arc::clone(&mtx);
        let _ = thread::spawn(move || {
            let _g = mtx_clone.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        let err = lock(&mtx, "db event listeners").expect_err("expected poison Err");
        let msg = err.to_string();
        assert!(
            msg.contains("db event listeners"),
            "lock err must name subsystem, got: {msg}",
        );
    }

    /// Recover-in-place policy (`data::registry` hot-path reads + init
    /// writes use `raw.write()/read().unwrap_or_else(|e| e.into_inner())`):
    /// the registry keeps working through a poisoned lock rather than
    /// panicking, and the recovered guard is fully usable.
    #[test]
    fn into_inner_recovers_a_poisoned_lock() {
        let rw = Arc::new(RwLock::new(vec![1u32, 2, 3]));
        poison_rw(&rw);
        rw.write().unwrap_or_else(|e| e.into_inner()).push(4);
        let snapshot = rw.read().unwrap_or_else(|e| e.into_inner()).clone();
        assert_eq!(snapshot, vec![1, 2, 3, 4], "recovered guard must be usable");
    }
}
