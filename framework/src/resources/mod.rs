//! JSON:API resource layer built on `#[derive(Data)]`.
//!
//! # Usage
//!
//! Add `#[json_resource("type")]` to a `#[derive(Data)]` struct to emit
//! an `IntoJsonResource` impl alongside the standard Inertia and serde impls.
//!
//! ```rust,ignore
//! #[derive(Debug, Clone, Data)]
//! #[json_resource("users")]
//! pub struct UserResource {
//!     pub id: i64,
//!     pub email: String,
//!     #[data(input_only)]
//!     pub password: String,
//! }
//!
//! // In a handler:
//! let user = UserResource { /* ... */ };
//! Resource::single(user).render().await
//! ```

pub mod builder;
pub mod errors;
pub mod fieldset;
pub mod include_tree;
pub mod response;
pub mod trait_def;

pub use builder::{JsonApiBuilder, render_resource_object};
pub use fieldset::{current_fieldset, scope_fieldset, RequestFieldsetSet, REQUEST_FIELDSET};
pub use include_tree::IncludeTree;
pub use response::{JsonApiResponse, Resource};
pub use trait_def::{
    IncludeResolutionError, IntoJsonResource, RelationshipValue, ResourceIdentifier,
};

use serde_json::Value;
use trait_def::IncludeResolutionError as IRE;

// ── AsRelationshipValue ─────────────────────────────────────────────────────

/// Type-class for "this field can produce a JSON:API relationship value."
/// Implemented for `T`, `Option<T>`, and `Vec<T>` where `T: IntoJsonResource`.
///
/// `Prop<T>` is intentionally excluded — it is Inertia-only and does not
/// satisfy JSON:API's requirement that relationship objects carry `(type, id)`
/// identifiers even when the related resource is not included.
pub trait AsRelationshipValue {
    fn as_relationship_value(&self) -> Option<RelationshipValue>;
}

impl<T: IntoJsonResource> AsRelationshipValue for T {
    fn as_relationship_value(&self) -> Option<RelationshipValue> {
        Some(RelationshipValue::Single(ResourceIdentifier::new(
            T::resource_type().to_string(),
            self.resource_id(),
        )))
    }
}

impl<T: IntoJsonResource> AsRelationshipValue for Vec<T> {
    fn as_relationship_value(&self) -> Option<RelationshipValue> {
        Some(RelationshipValue::Many(
            self.iter()
                .map(|t| {
                    ResourceIdentifier::new(T::resource_type().to_string(), t.resource_id())
                })
                .collect(),
        ))
    }
}

impl<T: IntoJsonResource> AsRelationshipValue for Option<T> {
    fn as_relationship_value(&self) -> Option<RelationshipValue> {
        match self {
            Some(t) => t.as_relationship_value(),
            None => Some(RelationshipValue::Null),
        }
    }
}

// ── PushIncluded ────────────────────────────────────────────────────────────

/// Type-class for "push my fully-resolved related resource(s) into
/// the `included` collection, recursively descending into the
/// include subtree for nested resources."
pub trait PushIncluded {
    fn push_included(
        &self,
        subtree: &IncludeTree,
        out: &mut Vec<Value>,
    ) -> Result<(), IRE>;
}

impl<T: IntoJsonResource> PushIncluded for T {
    fn push_included(
        &self,
        subtree: &IncludeTree,
        out: &mut Vec<Value>,
    ) -> Result<(), IRE> {
        let fieldset = current_fieldset();
        out.push(render_resource_object(self, &fieldset));
        // Recurse: resolve this resource's own includes per the subtree.
        if !subtree.is_empty() {
            self.resource_included(subtree, out)?;
        }
        Ok(())
    }
}

impl<T: IntoJsonResource> PushIncluded for Vec<T> {
    fn push_included(
        &self,
        subtree: &IncludeTree,
        out: &mut Vec<Value>,
    ) -> Result<(), IRE> {
        let fieldset = current_fieldset();
        for t in self {
            out.push(render_resource_object(t, &fieldset));
            if !subtree.is_empty() {
                t.resource_included(subtree, out)?;
            }
        }
        Ok(())
    }
}

impl<T: IntoJsonResource> PushIncluded for Option<T> {
    fn push_included(
        &self,
        subtree: &IncludeTree,
        out: &mut Vec<Value>,
    ) -> Result<(), IRE> {
        if let Some(t) = self {
            let fieldset = current_fieldset();
            out.push(render_resource_object(t, &fieldset));
            if !subtree.is_empty() {
                t.resource_included(subtree, out)?;
            }
        }
        Ok(())
    }
}
