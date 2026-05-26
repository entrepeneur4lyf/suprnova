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
