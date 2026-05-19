//! Process-wide allowlist of `?include=`-eligible fields per DTO.
//!
//! Populated at link time by `#[derive(Data)]` via the `inventory`
//! crate (one `inventory::submit!` per struct). The first call to
//! `is_allowed`/`allowed_for` drains the inventory collection into
//! the runtime map via `ensure_initialized`. The `register(name,
//! fields)` helper below stays available for tests that need to inject
//! ad-hoc allowlists. Default-deny: a DTO not registered allows nothing.

use crate::lock;
use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;

static REGISTRY: Lazy<RwLock<HashMap<&'static str, Vec<&'static str>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Entry submitted by `#[derive(Data)]` via `inventory::submit!`. Each
/// derived struct contributes one of these listing its `#[data(allow_include)]`
/// fields. The collection is drained into the runtime `REGISTRY` map by
/// `ensure_initialized` on first lookup.
pub struct AllowedIncludes {
    pub struct_name: &'static str,
    pub fields: &'static [&'static str],
}

inventory::collect!(AllowedIncludes);

fn ensure_initialized() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let mut map = lock::write(&REGISTRY).expect("data::registry REGISTRY write lock poisoned");
        for entry in inventory::iter::<AllowedIncludes> {
            map.insert(entry.struct_name, entry.fields.to_vec());
        }
    });
}

/// Register the allowed-include list for a DTO. Idempotent — re-registering
/// the same struct overwrites the prior list.
///
/// Calls `ensure_initialized()` first so that the inventory-collected entries
/// (from `#[derive(Data)]` link-time submissions) are drained into the map
/// before the manual write. This guarantees that a caller's explicit
/// registration is the final, authoritative state — it is never silently
/// overwritten by a later `ensure_initialized` call (the `Once` guard
/// prevents that anyway, but the pre-drain makes the ordering explicit).
///
/// This is the primary test-injection path for ad-hoc allowlists.
pub fn register(struct_name: &'static str, fields: &'static [&'static str]) {
    ensure_initialized();
    let mut map = lock::write(&REGISTRY)
        .expect("data::registry REGISTRY write lock poisoned");
    map.insert(struct_name, fields.to_vec());
}

/// Check whether `field` is includable on `struct_name`.
pub fn is_allowed(struct_name: &str, field: &str) -> bool {
    ensure_initialized();
    lock::read(&REGISTRY)
        .expect("data::registry REGISTRY read lock poisoned")
        .get(struct_name)
        .map(|fields| fields.contains(&field))
        .unwrap_or(false)
}

/// Returns the full allowed-include list for a DTO. Empty when the DTO
/// has not been registered.
pub fn allowed_for(struct_name: &str) -> Vec<&'static str> {
    ensure_initialized();
    lock::read(&REGISTRY)
        .expect("data::registry REGISTRY read lock poisoned")
        .get(struct_name)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_fields_are_allowed() {
        register("_test_MyDto", &["author", "tags"]);
        assert!(is_allowed("_test_MyDto", "author"));
        assert!(is_allowed("_test_MyDto", "tags"));
        assert!(!is_allowed("_test_MyDto", "secret"));
    }

    #[test]
    fn unregistered_dto_allows_nothing() {
        assert!(!is_allowed("_test_Unregistered", "anything"));
    }

    #[test]
    fn allowed_list_returns_registered() {
        register("_test_ListDto", &["a", "b", "c"]);
        let allowed = allowed_for("_test_ListDto");
        assert_eq!(allowed, vec!["a", "b", "c"]);
    }

    #[test]
    fn allowed_list_empty_when_unregistered() {
        assert!(allowed_for("_test_MissingDto").is_empty());
    }

    #[test]
    fn overwrite_replaces_prior_fields() {
        register("_test_OverwriteDto", &["old"]);
        register("_test_OverwriteDto", &["new"]);
        assert!(is_allowed("_test_OverwriteDto", "new"));
        assert!(!is_allowed("_test_OverwriteDto", "old"));
    }

    #[test]
    fn empty_allowlist_allows_nothing() {
        register("_test_EmptyDto", &[]);
        assert!(!is_allowed("_test_EmptyDto", "anything"));
        assert!(allowed_for("_test_EmptyDto").is_empty());
    }
}
