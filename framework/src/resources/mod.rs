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
//!
//! # Laravel parity
//!
//! - `Resource::single` / `Resource::collection` / `Resource::paginated`
//!   are the Suprnova analogues of Laravel's `JsonResource::make`,
//!   `JsonResource::collection`, and the paginated dispatch in
//!   `ResourceCollection::toResponse`.
//! - [`JsonApi`] is a Laravel-shape alias for [`Resource`].
//! - Per-response chainable mutators ([`JsonApiResponse::additional`],
//!   [`JsonApiResponse::with_meta`], [`JsonApiResponse::with_link`],
//!   [`JsonApiResponse::with_jsonapi`], [`JsonApiResponse::status`])
//!   mirror Laravel's `additional()`, `with()`, and
//!   `ResourceResponse::calculateStatus` family.
//! - [`Maybe`] / [`MissingValue`] are the conditional-attribute pattern
//!   from `Illuminate\Http\Resources\ConditionallyLoadsAttributes`.
//! - [`JsonApiInfo`] is the typed form of Laravel's
//!   `JsonApiResource::configure(version, ext, profile, meta)` call.

pub mod builder;
pub mod errors;
pub mod fieldset;
pub mod include_tree;
pub mod jsonapi_info;
pub mod maybe;
pub mod response;
pub mod trait_def;

pub use builder::{IncludedSink, JsonApiBuilder, render_resource_object};
pub use fieldset::{REQUEST_FIELDSET, RequestFieldsetSet, current_fieldset, scope_fieldset};
pub use include_tree::IncludeTree;
pub use jsonapi_info::JsonApiInfo;
pub use maybe::{Maybe, MissingValue, insert_maybe, strip_missing_values};
pub use response::{JsonApi, JsonApiResponse, Resource};
pub use trait_def::{
    IncludeResolutionError, IntoJsonResource, RelationshipValue, ResourceIdentifier,
};

use trait_def::IncludeResolutionError as IRE;

// ── AsRelationshipValue ─────────────────────────────────────────────────────

/// Type-class for "this field can produce a JSON:API relationship value."
/// Implemented for `T`, `Option<T>`, and `Vec<T>` where `T: IntoJsonResource`.
///
/// `Prop<T>` is intentionally excluded — it is Inertia-only and does not
/// satisfy JSON:API's requirement that relationship objects carry `(type, id)`
/// identifiers even when the related resource is not included.
pub trait AsRelationshipValue {
    /// Render `self` as a JSON:API relationship value — `Single`,
    /// `Many`, or `Null` — or `None` if this type cannot supply a
    /// relationship at all (e.g. an unloaded `Prop`).
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
                .map(|t| ResourceIdentifier::new(T::resource_type().to_string(), t.resource_id()))
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
///
/// `sink` deduplicates by `(type, id)` at push time, so a large
/// collection that shares relationships across items never materialises
/// the duplicates — peak memory and CPU stay proportional to the
/// distinct included resources, not the relationship fan-in.
pub trait PushIncluded {
    /// Push the fully-resolved included resource(s) for `self` into
    /// `sink`, recursively descending into `subtree` for nested
    /// resources. `sink` deduplicates by `(type, id)`.
    fn push_included(&self, subtree: &IncludeTree, sink: &mut IncludedSink) -> Result<(), IRE>;
}

impl<T: IntoJsonResource> PushIncluded for T {
    fn push_included(&self, subtree: &IncludeTree, sink: &mut IncludedSink) -> Result<(), IRE> {
        let fieldset = current_fieldset();
        sink.push(render_resource_object(self, &fieldset));
        // Recurse: resolve this resource's own includes per the subtree.
        if !subtree.is_empty() {
            self.resource_included(subtree, sink)?;
        }
        Ok(())
    }
}

impl<T: IntoJsonResource> PushIncluded for Vec<T> {
    fn push_included(&self, subtree: &IncludeTree, sink: &mut IncludedSink) -> Result<(), IRE> {
        let fieldset = current_fieldset();
        for t in self {
            sink.push(render_resource_object(t, &fieldset));
            if !subtree.is_empty() {
                t.resource_included(subtree, sink)?;
            }
        }
        Ok(())
    }
}

impl<T: IntoJsonResource> PushIncluded for Option<T> {
    fn push_included(&self, subtree: &IncludeTree, sink: &mut IncludedSink) -> Result<(), IRE> {
        if let Some(t) = self {
            let fieldset = current_fieldset();
            sink.push(render_resource_object(t, &fieldset));
            if !subtree.is_empty() {
                t.resource_included(subtree, sink)?;
            }
        }
        Ok(())
    }
}
