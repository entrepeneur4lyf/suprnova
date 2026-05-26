//! Process-global disk registry for the storage facade.
//!
//! Disks are registered once at boot via `Storage::register_*` and looked up
//! through `Storage::disk(name)`. The registry stores cloneable `opendal::Operator`
//! values so callers receive the full opendal surface (write, read, writer,
//! reader, presign_*, list, stat) without us having to proxy each method.
//!
//! Internally we use an `RwLock<Option<HashMap<...>>>` so the static can be
//! constructed in const context (stable since Rust 1.63) without `OnceLock`
//! gymnastics. The `Option` lets the testing helper wipe state between tests.

use crate::FrameworkError;
use opendal::Operator;
use std::collections::HashMap;
use std::sync::RwLock;

static REGISTRY: RwLock<Option<HashMap<String, Operator>>> = RwLock::new(None);

/// Register an `Operator` under `name`, replacing any previous registration.
pub(crate) fn register(name: impl Into<String>, op: Operator) {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    map.insert(name.into(), op);
}

/// Fetch a registered `Operator` by name.
pub(crate) fn get(name: &str) -> Result<Operator, FrameworkError> {
    let guard = REGISTRY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .and_then(|m| m.get(name).cloned())
        .ok_or_else(|| FrameworkError::internal(format!("storage disk '{name}' not registered")))
}

/// Wipe the registry. Only available under `cfg(test)` or with the `testing`
/// feature so production code cannot accidentally clear registered disks.
#[cfg(any(test, feature = "testing"))]
pub(crate) fn reset() {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = None;
}
