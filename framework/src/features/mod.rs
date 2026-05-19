//! Feature flags built on the `featureflag` crate.
//!
//! Re-exports `featureflag`'s primitives ([`Feature`], [`Context`],
//! [`Evaluator`]) so consumers reach for `suprnova::features::*`
//! without naming the upstream crate. Phase 13 layers a
//! SeaORM-backed [`DatabaseEvaluator`] (ships in Task 3), a Cache-
//! backed [`CachedEvaluator`] (Task 4), a per-request
//! [`FeatureMiddleware`] (Task 5), and an admin CRUD facade for the
//! `features` table (Task 6) on top.
//!
//! # Quick start
//!
//! ```ignore
//! use suprnova::features::{Context, Feature};
//! use suprnova::{context, feature, is_enabled};
//!
//! // Define a flag (typically a `const` at module scope).
//! pub const NEW_CHECKOUT: Feature<'static> =
//!     Feature::new("new-checkout", false);
//!
//! // Inside a request with `FeatureMiddleware` installed, the
//! // ambient `Context` carries user_id / team / roles already.
//! if is_enabled!("new-checkout", false) {
//!     // ... new behaviour
//! }
//!
//! // Out-of-request paths supply context explicitly:
//! let ctx = context! { user_id: 42i64 };
//! if NEW_CHECKOUT.is_enabled_in(Some(&ctx)) {
//!     // ...
//! }
//! ```
//!
//! # Why `featureflag`
//!
//! featureflag ships the lock-free `Arc<dyn Evaluator>` snapshot
//! model, the multi-tier scope stack (global / thread-local /
//! scope-local), the composable evaluator chain (`Filter`, `Chain`),
//! const-evaluable `Feature` definitions, and the
//! `is_enabled!` / `feature!` / `context!` macros. We add persistence
//! and middleware; reinventing the primitives layer would have been
//! significantly more work for no benefit. See `docs/core/feature-flags.md`
//! for end-to-end usage.

pub use featureflag::{
    context::Context,
    evaluator::{set_global_default, try_set_global_default, Evaluator, EvaluatorRef},
    feature::Feature,
};

pub mod entity;
pub mod evaluators;
pub mod fields;
pub mod migrations;

pub use evaluators::database::DatabaseEvaluator;
pub use fields::{TeamField, UserIdField};
