//! Recurring-subscription trait for payment providers.

use crate::payments::{
    PaymentResult, SubscribeRequest, SubscriptionResult, UpdateSubscriptionRequest,
};
use async_trait::async_trait;

/// Subscription management surface. Implemented by every provider that
/// supports recurring billing.
///
/// Merchant-of-Record providers (Paddle, etc.) and gateway providers
/// (Stripe, etc.) both implement this — the differentiation is the
/// optional [`super::payment::Payment`] trait, not this one.
#[async_trait]
pub trait Subscription: Send + Sync {
    /// Create a new subscription for the customer described in `req`.
    async fn subscribe(&self, req: SubscribeRequest) -> PaymentResult<SubscriptionResult>;
    /// Modify an existing subscription (price changes, scheduled cancel,
    /// etc.). Providers that don't support a given change return
    /// [`super::super::PaymentError::NotSupported`].
    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult>;
    /// Cancel a subscription. `at_period_end = true` schedules the
    /// cancellation for the end of the current billing period;
    /// `false` cancels immediately.
    async fn cancel(
        &self,
        provider_subscription_id: &str,
        at_period_end: bool,
    ) -> PaymentResult<SubscriptionResult>;
    /// Fetch the current state of a provider-side subscription. Used by
    /// reconciliation jobs and webhook fall-throughs to refresh mirror rows.
    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult>;
}
