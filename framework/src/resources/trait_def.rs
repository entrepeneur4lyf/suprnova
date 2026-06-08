//! Core trait definitions for the JSON:API resource layer.

use super::builder::IncludedSink;
use super::include_tree::IncludeTree;
use serde_json::{Map, Value};

/// Implemented by `#[derive(Data)] #[json_resource("<type>")]` types.
///
/// Mirrors the JSON:API resource object shape: every resource has a
/// `type`, a stringified `id`, an `attributes` object, and optional
/// `relationships`, `links`, and `meta` members. The derive macro emits
/// this impl from the field set; `resource_links` and `resource_meta`
/// have empty defaults that hand-rolled impls can override.
pub trait IntoJsonResource: Send + Sync {
    /// JSON:API `type` member (always identical across instances).
    fn resource_type() -> &'static str
    where
        Self: Sized;

    /// Stringified `id`. JSON:API spec requires `id` to be a string
    /// regardless of how it's stored.
    fn resource_id(&self) -> String;

    /// The `attributes` object — all non-relationship, non-id fields,
    /// filtered by the optional sparse-fieldset allowlist.
    fn resource_attributes(&self, fieldset: Option<&[&str]>) -> Value;

    /// The `relationships` object: maps relationship name → resource
    /// identifier or array of resource identifiers. Always emitted
    /// regardless of `?include=` (spec §5.2.1 — relationship object
    /// must be present with `data` linkage).
    fn resource_relationships(&self) -> Vec<(String, RelationshipValue)>;

    /// Push fully-resolved related resources into `sink` for the
    /// `included` compound document, walking the include tree
    /// recursively. Returns Err with the failing path if any
    /// requested include can't be resolved by this resource's
    /// allow_include allowlist.
    ///
    /// `sink` deduplicates by `(type, id)` at push time per
    /// JSON:API spec section 8, so callers do not need to filter
    /// duplicates afterwards.
    fn resource_included(
        &self,
        include_tree: &IncludeTree,
        sink: &mut IncludedSink,
    ) -> Result<(), IncludeResolutionError>;

    /// Optional per-resource `links` member (spec §5.2.7). Empty by
    /// default; override to provide `self`, `related`, etc. links on
    /// this specific resource object.
    ///
    /// Mirrors Laravel's `JsonApiResource::toLinks(Request)`.
    fn resource_links(&self) -> Map<String, Value> {
        Map::new()
    }

    /// Optional per-resource `meta` member (spec §5.2.7). Empty by
    /// default; override to provide non-standard metadata that is
    /// specific to this resource instance.
    ///
    /// Mirrors Laravel's `JsonApiResource::toMeta(Request)`.
    fn resource_meta(&self) -> Map<String, Value> {
        Map::new()
    }

    /// Optional top-level `meta` member contributed by this resource
    /// during rendering (spec §5.1.2). Empty by default. Used when a
    /// resource itself wants to attach top-level metadata regardless
    /// of how the response is constructed.
    ///
    /// Mirrors Laravel's `JsonResource::with(Request)`.
    fn resource_top_level_meta(&self) -> Map<String, Value> {
        Map::new()
    }
}

/// A JSON:API relationship value.
#[derive(Debug, Clone)]
pub enum RelationshipValue {
    /// `{"data": {"type": "...", "id": "..."}}`
    Single(ResourceIdentifier),
    /// `{"data": [{"type": "...", "id": "..."}, ...]}`
    Many(Vec<ResourceIdentifier>),
    /// `{"data": null}`
    Null,
}

/// JSON:API resource identifier — the (type, id) pair that appears
/// inside `relationships.<name>.data`.
#[derive(Debug, Clone)]
pub struct ResourceIdentifier {
    /// JSON:API `type` value identifying the resource kind.
    pub resource_type: String,
    /// Resource id, stringified per the JSON:API spec.
    pub id: String,
}

impl ResourceIdentifier {
    /// Construct an identifier from a type name and id.
    pub fn new(resource_type: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            id: id.into(),
        }
    }

    /// Render the identifier as the `{"type": …, "id": …}` JSON object the
    /// JSON:API spec embeds in relationship data and the `included` array.
    pub fn to_value(&self) -> Value {
        serde_json::json!({
            "type": self.resource_type,
            "id": self.id,
        })
    }
}

/// Returned by `resource_included` when the request's include tree
/// names a relationship not on this resource's allowlist. Rendered
/// to a JSON:API 400 errors envelope.
#[derive(Debug, Clone)]
pub struct IncludeResolutionError {
    /// Dotted include path that couldn't be resolved (e.g. `author.posts.comments`).
    pub path: String,
    /// JSON:API `type` of the resource the unresolved segment was queried on.
    pub on_type: &'static str,
}
