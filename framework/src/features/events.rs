//! Feature-flag events.
//!
//! Two events fire from the admin CRUD path. `is_enabled` resolution
//! deliberately does **not** fire an event — every request that
//! checks any flag would multiply that event volume by N flags
//! checked. Production deployments that need read-path audit can
//! wire a custom `Evaluator` wrapper with a bounded log channel
//! (out of v1 scope; documented in `manual/feature-flags.md`).

use crate::events::Event;

/// Fires when an operator (or any caller of
/// [`crate::features::admin::upsert`]) creates or updates a flag.
///
/// `actor_id` is the user id that performed the change, if known.
/// `NULL` means "system-initiated" (e.g. a CLI bootstrap or
/// migration seed). String-typed to carry torii's opaque user ids.
#[derive(Debug, Clone)]
pub struct FeatureUpdated {
    pub name: String,
    pub scope_key: String,
    pub enabled: bool,
    pub actor_id: Option<String>,
}

impl Event for FeatureUpdated {
    fn event_name() -> &'static str {
        "FeatureUpdated"
    }
}

/// Fires when an operator deletes a flag row entirely (via
/// [`crate::features::admin::delete`]). Distinct from
/// [`FeatureUpdated`] because "row removed" is semantically
/// different from "value flipped to false": after a delete, the
/// flag falls back to its compiled-in default; after a `false`
/// upsert, the explicit-disabled state remains in storage.
#[derive(Debug, Clone)]
pub struct FeatureDeleted {
    pub name: String,
    pub scope_key: String,
    pub actor_id: Option<String>,
}

impl Event for FeatureDeleted {
    fn event_name() -> &'static str {
        "FeatureDeleted"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::events::Event;

    #[test]
    fn event_names_are_unique() {
        let names = [FeatureUpdated::event_name(), FeatureDeleted::event_name()];
        let mut sorted = names.to_vec();
        sorted.sort();
        sorted.dedup();
        assert_eq!(sorted.len(), names.len());
    }
}
