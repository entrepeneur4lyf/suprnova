//! Process-wide allowlist of `?include=`-eligible fields per DTO.
//!
//! Populated at link time by `#[derive(Data)]` via the `inventory`
//! crate (one `inventory::submit!` per struct). The first call to
//! `is_allowed`/`allowed_for` drains the inventory collection into
//! the runtime map (see `ensure_initialized` in Task 8). The
//! `register(name, fields)` helper below stays available for tests
//! that need to inject ad-hoc allowlists. Default-deny: a DTO not
//! registered allows nothing.

use once_cell::sync::Lazy;
use std::collections::HashMap;
use std::sync::RwLock;

static REGISTRY: Lazy<RwLock<HashMap<&'static str, Vec<&'static str>>>> =
    Lazy::new(|| RwLock::new(HashMap::new()));

/// Register the allowed-include list for a DTO. Idempotent — re-registering
/// the same struct overwrites the prior list.
pub fn register(struct_name: &'static str, fields: &'static [&'static str]) {
    let mut map = REGISTRY.write().unwrap();
    map.insert(struct_name, fields.to_vec());
}

/// Check whether `field` is includable on `struct_name`.
pub fn is_allowed(struct_name: &str, field: &str) -> bool {
    REGISTRY
        .read()
        .unwrap()
        .get(struct_name)
        .map(|fields| fields.iter().any(|f| *f == field))
        .unwrap_or(false)
}

/// Returns the full allowed-include list for a DTO. Empty when the DTO
/// has not been registered.
pub fn allowed_for(struct_name: &str) -> Vec<&'static str> {
    REGISTRY
        .read()
        .unwrap()
        .get(struct_name)
        .cloned()
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registered_fields_are_allowed() {
        register("MyDto", &["author", "tags"]);
        assert!(is_allowed("MyDto", "author"));
        assert!(is_allowed("MyDto", "tags"));
        assert!(!is_allowed("MyDto", "secret"));
    }

    #[test]
    fn unregistered_dto_allows_nothing() {
        assert!(!is_allowed("Unregistered", "anything"));
    }

    #[test]
    fn allowed_list_returns_registered() {
        register("ListDto", &["a", "b", "c"]);
        let allowed = allowed_for("ListDto");
        assert_eq!(allowed, vec!["a", "b", "c"]);
    }

    #[test]
    fn allowed_list_empty_when_unregistered() {
        assert!(allowed_for("MissingDto").is_empty());
    }
}
