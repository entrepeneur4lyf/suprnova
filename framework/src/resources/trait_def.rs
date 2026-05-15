//! Core trait definitions for the JSON:API resource layer.

use serde_json::Value;
use super::include_tree::IncludeTree;

/// Implemented by `#[derive(Data)] #[json_resource("<type>")]` types.
///
/// Mirrors the JSON:API resource object shape: every resource has a
/// `type`, a stringified `id`, an `attributes` object, and optional
/// `relationships`. The derive macro emits this impl from the field set.
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

    /// Push fully-resolved related resources into `out` for the
    /// `included` compound document, walking the include tree
    /// recursively. Returns Err with the failing path if any
    /// requested include can't be resolved by this resource's
    /// allow_include allowlist.
    fn resource_included(
        &self,
        include_tree: &IncludeTree,
        out: &mut Vec<Value>,
    ) -> Result<(), IncludeResolutionError>;
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
    pub resource_type: String,
    pub id: String,
}

impl ResourceIdentifier {
    pub fn new(resource_type: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            resource_type: resource_type.into(),
            id: id.into(),
        }
    }

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
    pub path: String,
    pub on_type: &'static str,
}
