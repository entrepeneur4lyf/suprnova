//! Small helpers to handle poisoned locks consistently across the framework.
//!
//! Policy (2026-05):
//! - We treat a poisoned lock as an internal error.
//! - Callers should almost always get a `FrameworkError` instead of panicking.
//! - This prevents one bad request from taking down an entire subsystem.

use std::sync::{Mutex, MutexGuard, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::error::FrameworkError;

/// Acquire a read guard on an `RwLock`, returning a `FrameworkError` on poison.
pub(crate) fn read<T>(lock: &RwLock<T>) -> Result<RwLockReadGuard<'_, T>, FrameworkError> {
    lock.read()
        .map_err(|_| FrameworkError::internal("internal registry lock poisoned"))
}

/// Acquire a write guard on an `RwLock`, returning a `FrameworkError` on poison.
pub(crate) fn write<T>(lock: &RwLock<T>) -> Result<RwLockWriteGuard<'_, T>, FrameworkError> {
    lock.write()
        .map_err(|_| FrameworkError::internal("internal registry lock poisoned"))
}

/// Acquire a guard on a `Mutex`, returning a `FrameworkError` on poison.
pub(crate) fn lock<T>(lock: &Mutex<T>) -> Result<MutexGuard<'_, T>, FrameworkError> {
    lock.lock()
        .map_err(|_| FrameworkError::internal("internal registry lock poisoned"))
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

    /// Propagate policy (the #371 registration writes — notifications and
    /// mail registries `?` these helpers): a poisoned lock surfaces as a
    /// `FrameworkError` rather than panicking the whole subsystem.
    #[test]
    fn helpers_return_err_on_poison_instead_of_panicking() {
        let rw = Arc::new(RwLock::new(0u32));
        poison_rw(&rw);
        assert!(read(&rw).is_err(), "lock::read must return Err on poison");
        assert!(write(&rw).is_err(), "lock::write must return Err on poison");

        let mtx = Arc::new(Mutex::new(0u32));
        let mtx_clone = Arc::clone(&mtx);
        let _ = thread::spawn(move || {
            let _g = mtx_clone.lock().unwrap();
            panic!("intentional poison");
        })
        .join();
        assert!(mtx.is_poisoned(), "test setup: Mutex must be poisoned");
        assert!(lock(&mtx).is_err(), "lock::lock must return Err on poison");
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
