//! Process-wide allowlist of `?include=`-eligible fields per DTO.
//!
//! Populated at link time by `#[derive(Data)]` via the `inventory`
//! crate (one `inventory::submit!` per struct). The first call to
//! `is_allowed`/`allowed_for` drains the inventory collection into
//! the runtime map via `ensure_initialized`. The `register(name,
//! fields)` helper below stays available for tests that need to inject
//! ad-hoc allowlists. Default-deny: a DTO not registered allows nothing.
//!
//! # Key shape — fully-qualified type names
//!
//! Keys are the fully-qualified type name produced by
//! `concat!(module_path!(), "::", stringify!(StructName))` — the same
//! expression the derive macro emits at the `inventory::submit!` call
//! site. This prevents collisions between two same-named DTOs in
//! different modules.
//!
//! Callers — both for `register` (writes) and `is_allowed` /
//! `allowed_for` (reads) — MUST use the same key shape:
//!
//! ```ignore
//! // Correct: matches what `#[derive(Data)]` writes for `crate::dto::AlbumDto`.
//! const KEY: &str = concat!(module_path!(), "::", "AlbumDto");
//! registry::register(KEY, &["songs", "artist"]);
//! assert!(registry::is_allowed(KEY, "songs"));
//! ```
//!
//! Bare struct names (`"AlbumDto"`) will silently miss every lookup —
//! the registry treats them as a different key entirely.

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
    /// Fully-qualified name of the deriving struct (`Module::Foo` form).
    pub struct_name: &'static str,
    /// Field paths the struct opted into via `#[data(allow_include)]`.
    pub fields: &'static [&'static str],
}

inventory::collect!(AllowedIncludes);

fn ensure_initialized() {
    static INIT: std::sync::Once = std::sync::Once::new();
    INIT.call_once(|| {
        let mut map = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
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
///
/// # `struct_name` must be a fully-qualified type name
///
/// The derive macro writes keys as
/// `concat!(module_path!(), "::", stringify!(StructName))`. Manual
/// callers MUST use the same key shape, or `is_allowed` / `allowed_for`
/// lookups will silently miss. See the [module docs](self) for the
/// rationale and an example. A bare struct name like `"AlbumDto"`
/// will register a "ghost" entry that no real lookup will ever hit.
pub fn register(struct_name: &'static str, fields: &'static [&'static str]) {
    ensure_initialized();
    let mut map = REGISTRY.write().unwrap_or_else(|e| e.into_inner());
    map.insert(struct_name, fields.to_vec());
}

/// Check whether `field` is includable on `struct_name`. The
/// `struct_name` must be the fully-qualified type name — see the
/// [module docs](self) for the key shape.
pub fn is_allowed(struct_name: &str, field: &str) -> bool {
    ensure_initialized();
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(struct_name)
        .map(|fields| fields.contains(&field))
        .unwrap_or(false)
}

/// Returns the full allowed-include list for a DTO. Empty when the DTO
/// has not been registered. The `struct_name` must be the
/// fully-qualified type name — see the [module docs](self) for the
/// key shape.
pub fn allowed_for(struct_name: &str) -> Vec<&'static str> {
    ensure_initialized();
    REGISTRY
        .read()
        .unwrap_or_else(|e| e.into_inner())
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
