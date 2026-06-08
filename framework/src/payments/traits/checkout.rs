//! Hosted-checkout-session trait for payment providers.

use crate::payments::{PaymentResult, SessionPayload, StartSessionRequest};
use async_trait::async_trait;

/// Hosted checkout flow. Implemented by every payment provider that
/// supports a redirect, popup, embed, or out-of-band confirmation flow
/// (which is to say: all of them).
///
/// Returns a [`SessionPayload`] tagged by `flow` so the frontend SDK can
/// dispatch to the right widget — Stripe Elements, Paddle inline,
/// Mobile Money prompt, generic redirect, etc.
#[async_trait]
pub trait Checkout: Send + Sync {
    /// Start a hosted checkout session for the request.
    ///
    /// The returned [`SessionPayload`] carries the provider-specific tokens
    /// the frontend needs to complete the flow.
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload>;
}
