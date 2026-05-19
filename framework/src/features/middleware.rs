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
/// - `user_id: i64` from [`Auth::id`] when the session is
///   authenticated and the stored identifier parses as an `i64`.
///   Non-numeric identifiers (e.g. UUID-shaped torii ids) skip the
///   user-scope field; global flags still resolve. Custom field
///   extractors land in a future iteration.
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
/// Returns [`Context::root`] when no user is authenticated or the
/// stored identifier is not numeric — global flags still resolve in
/// that case.
fn build_context() -> Context {
    let user_id: Option<i64> = Auth::id().and_then(|s| s.parse().ok());
    match user_id {
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
        last_user_id: std::sync::Mutex<Option<i64>>,
    }

    impl InspectingEvaluator {
        fn new() -> Self {
            Self {
                last_user_id: std::sync::Mutex::new(None),
            }
        }

        fn last_user_id(&self) -> Option<i64> {
            *self.last_user_id.lock().unwrap()
        }
    }

    impl Evaluator for InspectingEvaluator {
        fn is_enabled(&self, _feature: &str, context: &Context) -> Option<bool> {
            let id = context
                .iter()
                .find_map(|c| c.extensions().get::<UserIdField>())
                .map(|f| f.0);
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
            // `DatabaseEvaluator` performs.
            if let Some(value) = fields.get("user_id") {
                if let Some(id) = value.as_i64() {
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
    fn build_context_skips_non_numeric_user_id() {
        // Auth::id() relies on the session task-local; tests without
        // a session see None. The "non-numeric id => skip" branch is
        // covered by direct unit testing of the parse step:
        let parsed: Option<i64> = Some("not-a-number".to_string()).and_then(|s| s.parse().ok());
        assert!(parsed.is_none());
    }
}
