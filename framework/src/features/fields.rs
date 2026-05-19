//! Typed newtypes that an [`Evaluator`](featureflag::evaluator::Evaluator) stashes into a
//! [`Context`](featureflag::context::Context)'s [`Extensions`](featureflag::extensions::Extensions).
//!
//! featureflag's `context!` macro carries fields as a flat
//! `&[(&str, Value)]` slice at creation time. The macro then invokes
//! the active evaluator's [`on_new_context`](featureflag::evaluator::Evaluator::on_new_context)
//! hook, which is where evaluators translate the raw field slice into
//! `TypeId`-keyed values inside `Extensions`. Those values are what
//! [`is_enabled`](featureflag::evaluator::Evaluator::is_enabled) reads
//! on the hot path.
//!
//! We expose these newtypes publicly so:
//!
//! * downstream evaluators (and the upcoming `FeatureMiddleware` in
//!   Task 5) can populate `Extensions` themselves — anything that
//!   stashes a [`UserIdField`] participates in user-scoped flag
//!   resolution, regardless of which evaluator generated the context.
//! * consumers who construct contexts programmatically (without the
//!   `context!` macro) can `extensions_mut().insert(UserIdField(id))`
//!   directly when they want to bypass the field-slice indirection.
//!
//! ```ignore
//! use suprnova::features::fields::UserIdField;
//! use featureflag::context::Context;
//!
//! // Programmatic insertion (rare — most callers use the `context!` macro).
//! let mut ctx_ref: featureflag::context::ContextRef<'_> = /* ... */;
//! ctx_ref.extensions_mut().insert(UserIdField(42));
//! ```
//!
//! # Naming
//!
//! The `Field` suffix is intentional. `UserId` alone collides with
//! `torii::UserId`; the suffix makes it unambiguous that these are
//! feature-flag context fields, not domain identifiers.

/// Authenticated user identity carried in the feature-flag context.
///
/// Set from the `user_id` field of [`context!`](featureflag::context!)
/// when the value is an `i64` (or anything that `ToValue`s to one).
/// The [`DatabaseEvaluator`](crate::features::DatabaseEvaluator) reads
/// this to look up `user:{id}`-scoped flags.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct UserIdField(pub i64);

/// Team / organization the user belongs to in the feature-flag context.
///
/// Set from the `team` field of [`context!`](featureflag::context!)
/// when the value is a string. The
/// [`DatabaseEvaluator`](crate::features::DatabaseEvaluator) reads
/// this to look up `team:{name}`-scoped flags.
///
/// String-typed (not enum) so applications stay free to define their
/// own team taxonomy without coordinating with the framework.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct TeamField(pub String);
