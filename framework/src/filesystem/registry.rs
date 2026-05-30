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
//!
//! The registry is process-global. Disks are meant to be registered once at
//! boot; re-registering a name replaces the previous operator and emits a
//! `warn!` (an accidental duplicate could swap a production disk for a
//! local/memory one). Tests that exercise disks in parallel must isolate
//! through [`crate::filesystem::Storage::fake`], whose guard serializes against
//! other fake users and resets the registry on drop — calling `register_*`
//! directly from multiple parallel tests races on this global state.

use crate::FrameworkError;
use opendal::Operator;
use std::collections::HashMap;
use std::sync::RwLock;

static REGISTRY: RwLock<Option<HashMap<String, Operator>>> = RwLock::new(None);

/// Register an `Operator` under `name`, replacing any previous registration.
///
/// Disks are meant to be registered once at boot. Replacing an existing name
/// emits a `warn!` because it is almost always accidental (a duplicate name in
/// config, or a re-bootstrap) and can swap a production disk for a different
/// backend. The replacement still happens — this is a signal, not a hard error,
/// so legitimate re-registration (e.g. `Storage::fake` setup) keeps working.
pub(crate) fn register(name: impl Into<String>, op: Operator) {
    let name = name.into();
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let map = guard.get_or_insert_with(HashMap::new);
    if map.insert(name.clone(), op).is_some() {
        tracing::warn!(
            disk = %name,
            "storage disk re-registered; the previously registered operator for this name was replaced"
        );
    }
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

/// Drop a single named disk from the registry, returning whether it was
/// present. Mirrors Laravel's `FilesystemManager::forgetDisk`. Safe to call
/// from production code — applications occasionally need to drop and
/// re-register a disk at runtime (e.g. after a configuration reload).
pub(crate) fn forget(name: &str) -> bool {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_mut()
        .map(|m| m.remove(name).is_some())
        .unwrap_or(false)
}

/// Drop every registered disk. Mirrors Laravel's
/// `FilesystemManager::purge()` (which clears every disk when called
/// without arguments).
pub(crate) fn purge() {
    let mut guard = REGISTRY
        .write()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some(m) = guard.as_mut() {
        m.clear();
    }
}

/// Return the names of every currently-registered disk. Useful for
/// diagnostics, admin views, and tests.
pub(crate) fn names() -> Vec<String> {
    let guard = REGISTRY
        .read()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    guard
        .as_ref()
        .map(|m| {
            let mut v: Vec<String> = m.keys().cloned().collect();
            v.sort();
            v
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filesystem::testing::FAKE_LOCK;
    use opendal::services;

    // These tests share the process-global `REGISTRY` with every
    // `Storage::fake()` test in the suite. The fake guard resets the
    // registry on drop, so without coordination it can wipe a name
    // between this test's two `register()` calls and mask the expected
    // warn. Holding `FAKE_LOCK` for the duration of these tests serializes
    // them against every fake user, eliminating the race window.
    fn memory_op() -> Operator {
        Operator::new(services::Memory::default())
            .expect("opendal memory service is infallible")
            .finish()
    }

    #[tracing_test::traced_test]
    #[test]
    fn re_registering_an_existing_name_warns() {
        let _lock = FAKE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        register("registry_dup_warn_probe", memory_op());
        register("registry_dup_warn_probe", memory_op());
        assert!(
            logs_contain("re-registered"),
            "replacing an already-registered disk name must emit a warn"
        );
    }

    #[tracing_test::traced_test]
    #[test]
    fn first_registration_of_a_name_does_not_warn() {
        let _lock = FAKE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        register("registry_fresh_no_warn_probe", memory_op());
        assert!(
            !logs_contain("re-registered"),
            "a first-time disk registration must not warn"
        );
    }
}
