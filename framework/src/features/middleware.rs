//! [`FeatureMiddleware`] — opens a per-request feature-flag
//! [`Context`](featureflag::context::Context) populated from the
//! active session, then runs the downstream handler inside that
//! context.
//!
//! Without this middleware in the chain, [`is_enabled!`] calls fall
//! back to the [`Context::root`](featureflag::context::Context::root)
//! context with no user / team scope — only global flags resolve.
//! With it installed, user-scoped flags lit up automatically based on
//! [`Auth::id`](crate::auth::Auth::id).
//!
//! # Async correctness
//!
//! featureflag's context stack is thread-local. A naive
//! `ctx.in_scope(|| next(request).await)` would lose the context when
//! the future suspends + resumes on another tokio worker. We use
//! featureflag's [`AnyExt::wrap_context`] adapter (gated behind the
//! `futures` Cargo feature, which is enabled by Phase 13 T1) so each
//! `poll` of the inner future re-enters the context scope. Context is
//! preserved across `.await` boundaries.

use crate::auth::Auth;
use crate::http::{Request, Response};
use crate::middleware::{Middleware, Next};
use async_trait::async_trait;
use featureflag::context::Context;
use std::future::Future;
use std::pin::Pin;
use std::task::{Context as TaskContext, Poll};

/// Opens a per-request featureflag [`Context`] populated from the
/// active session.
///
/// Composes after `SessionMiddleware` and `AuthMiddleware`/equivalent,
/// since `Auth::id()` reads the session-bound user identity. Place
/// this in your global middleware stack — `is_enabled!` calls in any
/// downstream handler then see the right scope automatically.
///
/// Currently extracts:
///
/// - `user_id: String` from [`Auth::id`] when the session is
///   authenticated. Carries torii's opaque (UUID/ULID) ids verbatim
///   and numeric ids in their stringified form — both shapes participate
///   in user-scoped flag resolution.
#[derive(Default)]
pub struct FeatureMiddleware {
    _private: (),
}

impl FeatureMiddleware {
    /// Construct a new middleware instance. Zero-config — the
    /// extractor pulls `user_id` from `Auth::id()` and that's it for
    /// v1. Future iterations expose an `extract_with` builder that
    /// lets applications register custom field-extractor closures
    /// (e.g. tenant id, role list).
    pub fn new() -> Self {
        Self { _private: () }
    }
}

#[async_trait]
impl Middleware for FeatureMiddleware {
    async fn handle(&self, request: Request, next: Next) -> Response {
        let ctx = build_context();
        InContext {
            context: ctx,
            inner: next(request),
        }
        .await
    }
}

/// Combinator that re-enters a featureflag [`Context`] on every poll
/// of the inner future. featureflag's stack is `thread_local`, so a
/// naive `ctx.in_scope(|| async {...}.await)` loses the context when
/// the future resumes on a different tokio worker. This combinator
/// re-applies the scope at each poll, mirroring the internal
/// behaviour of featureflag's `WrapContext` adapter (which we can't
/// use directly here because `featureflag::utils::AnyExt` lacks a
/// blanket impl and the orphan rule blocks us from adding one for
/// `Pin<Box<dyn Future + Send>>`).
struct InContext<F> {
    context: Context,
    inner: F,
}

impl<F: Future + Unpin> Future for InContext<F> {
    type Output = F::Output;

    fn poll(mut self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<Self::Output> {
        let this = &mut *self;
        let context = &this.context;
        let inner = Pin::new(&mut this.inner);
        context.in_scope(|| inner.poll(cx))
    }
}

/// Read the active session via `Auth::id()` and build a featureflag
/// [`Context`] capturing whatever scope fields we can resolve.
///
/// Returns [`Context::root`] when no user is authenticated — global
/// flags still resolve in that case.
fn build_context() -> Context {
    match Auth::id() {
        Some(id) => featureflag::context! { user_id = id },
        None => Context::root(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::fields::UserIdField;
    use featureflag::evaluator::{with_default, Evaluator};
    use std::sync::Arc;

    /// Inspector evaluator: records the user_id it sees on
    /// each `is_enabled` call.
    struct InspectingEvaluator {
        last_user_id: std::sync::Mutex<Option<String>>,
    }

    impl InspectingEvaluator {
        fn new() -> Self {
            Self {
                last_user_id: std::sync::Mutex::new(None),
            }
        }

        fn last_user_id(&self) -> Option<String> {
            self.last_user_id.lock().unwrap().clone()
        }
    }

    impl Evaluator for InspectingEvaluator {
        fn is_enabled(&self, _feature: &str, context: &Context) -> Option<bool> {
            let id = context
                .iter()
                .find_map(|c| c.extensions().get::<UserIdField>())
                .map(|f| f.0.clone());
            *self.last_user_id.lock().unwrap() = id;
            None
        }

        fn on_new_context(
            &self,
            mut context: featureflag::context::ContextRef<'_>,
            fields: featureflag::fields::Fields<'_>,
        ) {
            // Translate the raw `user_id` field into a typed
            // `UserIdField` extension — same translation
            // `DatabaseEvaluator` performs, including i64→String
            // coercion for numeric-keyed apps.
            if let Some(value) = fields.get("user_id") {
                let id = value
                    .as_str()
                    .map(String::from)
                    .or_else(|| value.as_i64().map(|i| i.to_string()));
                if let Some(id) = id {
                    context.extensions_mut().insert(UserIdField(id));
                }
            }
        }
    }

    #[test]
    fn build_context_skips_user_id_when_unauthenticated() {
        // No session installed — Auth::id() returns None.
        let evaluator = Arc::new(InspectingEvaluator::new());
        with_default(evaluator.clone(), || {
            let ctx = build_context();
            assert!(ctx.is_root(), "no session => root context");
            // Sanity-check that an inner is_enabled sees no user.
            evaluator.is_enabled("any", &ctx);
            assert_eq!(evaluator.last_user_id(), None);
        });
    }

    #[test]
    fn user_id_field_accepts_uuid_string_via_context_macro() {
        // Exercise the on_new_context coercion path that backs
        // build_context: a UUID-shaped string id flows through the
        // `context!` macro into a `UserIdField(String)`.
        let evaluator = Arc::new(InspectingEvaluator::new());
        with_default(evaluator.clone(), || {
            let uuid = "01HZK6V3J7Q5G4P8X9N2D1B0M3".to_string();
            let ctx = featureflag::context! { user_id = uuid.clone() };
            evaluator.is_enabled("any", &ctx);
            assert_eq!(
                evaluator.last_user_id(),
                Some(uuid),
                "uuid-shaped string ids must round-trip into UserIdField verbatim",
            );
        });
    }

    #[test]
    fn user_id_field_coerces_i64_via_context_macro() {
        // Numeric-keyed apps still get to write `user_id = 42i64` — the
        // coercion in on_new_context stringifies for storage.
        let evaluator = Arc::new(InspectingEvaluator::new());
        with_default(evaluator.clone(), || {
            let ctx = featureflag::context! { user_id = 42_i64 };
            evaluator.is_enabled("any", &ctx);
            assert_eq!(
                evaluator.last_user_id(),
                Some("42".to_string()),
                "i64 ids must coerce into the String form",
            );
        });
    }
}
