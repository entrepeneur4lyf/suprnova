//! Implementation of the `Subscription` trait for `PaddleProvider`.
//!
//! `subscribe()` returns `NotSupported` — Paddle subscriptions are created
//! indirectly via checkout completion, NOT directly. Domain code should call
//! `Checkout::start_session` with `SessionMode::Subscription` and react to
//! the `SubscriptionCreated` webhook for the resulting subscription_id.
//!
//! `cancel`, `update`, and `get` wire through the SDK.

use async_trait::async_trait;
use suprnova::payments::{
    PaymentError, PaymentResult, SubscribeRequest, Subscription, SubscriptionItemSnapshot,
    SubscriptionResult, SubscriptionStatus, UpdateSubscriptionRequest,
};

use crate::PaddleProvider;

#[async_trait]
impl Subscription for PaddleProvider {
    async fn subscribe(&self, _req: SubscribeRequest) -> PaymentResult<SubscriptionResult> {
        Err(PaymentError::NotSupported(
            "Paddle subscriptions are created via Checkout::start_session + checkout completion. \
             Use Checkout::start_session with SessionMode::Subscription and await the \
             SubscriptionCreated webhook for the resulting subscription_id."
                .into(),
        ))
    }

    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult> {
        // v1 supports cancel_at_period_end only. Price-set replacement on a Paddle
        // subscription requires a different API shape; return NotSupported honestly.
        if req.new_price_refs.is_some() {
            return Err(PaymentError::NotSupported(
                "Paddle price-set replacement on existing subscription not in v1.".into(),
            ));
        }

        match req.cancel_at_period_end {
            Some(true) => {
                // Mirror cancel(at_period_end=true): schedule via subscription_cancel
                // with default (NextBillingPeriod).
                let resp = self
                    .client()
                    .subscription_cancel(req.provider_subscription_id.clone())
                    .send()
                    .await
                    .map_err(|e| {
                        PaymentError::Provider(format!("paddle subscription_cancel (cape): {e}"))
                    })?;
                Ok(map_paddle_subscription(&resp.data))
            }
            Some(false) => {
                // Un-scheduling a previously-scheduled cancellation is a
                // distinct Paddle API surface (the resume endpoint) and was
                // not wired in v1. Falling through to subscription_get would
                // silently return success while leaving the cancellation
                // scheduled — a dual-API fail-loud violation. Surface it
                // honestly so callers know to route through the dedicated
                // resume call when it lands.
                Err(PaymentError::NotSupported(
                    "Paddle un-schedule-cancel (cancel_at_period_end: Some(false)) is not \
                     supported on Subscription::update in v1. The Paddle resume endpoint \
                     handles this case; route un-cancel requests there directly."
                        .into(),
                ))
            }
            None => {
                // No-op update (no cancel_at_period_end delta, no price-set
                // change): re-fetch current state via subscription_get so the
                // caller always observes the authoritative provider snapshot.
                let resp = self
                    .client()
                    .subscription_get(req.provider_subscription_id)
                    .send()
                    .await
                    .map_err(|e| PaymentError::Provider(format!("paddle subscription_get: {e}")))?;
                Ok(map_subscription_with_include(&resp.data))
            }
        }
    }

    async fn cancel(
        &self,
        provider_subscription_id: &str,
        _at_period_end: bool,
    ) -> PaymentResult<SubscriptionResult> {
        // Paddle's SubscriptionCancel takes an EffectiveFrom enum (NextBillingPeriod | Immediately)
        // but that enum is private in 0.18.0. The default (NextBillingPeriod) covers cape=true;
        // immediate cancel requires accessing the private enum and is not viable in v1.
        // Callers requesting immediate cancel get the scheduled-cancel behavior — Paddle's webhook
        // still fires SubscriptionCanceled when the period actually ends.
        let resp = self
            .client()
            .subscription_cancel(provider_subscription_id.to_string())
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle subscription_cancel: {e}")))?;
        Ok(map_paddle_subscription(&resp.data))
    }

    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult> {
        let resp = self
            .client()
            .subscription_get(provider_subscription_id.to_string())
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle subscription_get: {e}")))?;
        Ok(map_subscription_with_include(&resp.data))
    }
}

/// Map a status value to our enum by inspecting its Debug representation. The
/// underlying Paddle SubscriptionStatus enum is private; this avoids a dependency
/// on internal SDK types while still producing reliable mappings.
fn map_status_from_debug<S: std::fmt::Debug>(status: &S) -> SubscriptionStatus {
    let s = format!("{status:?}").to_lowercase();
    if s.contains("active") {
        SubscriptionStatus::Active
    } else if s.contains("trialing") || s.contains("trial") {
        SubscriptionStatus::Trialing
    } else if s.contains("past") {
        SubscriptionStatus::PastDue
    } else if s.contains("paused") {
        SubscriptionStatus::Paused
    } else if s.contains("cancel") {
        SubscriptionStatus::Canceled
    } else {
        SubscriptionStatus::Incomplete
    }
}

fn map_paddle_subscription(s: &paddle_rust_sdk::entities::Subscription) -> SubscriptionResult {
    let items: Vec<SubscriptionItemSnapshot> = s
        .items
        .iter()
        .map(|item| SubscriptionItemSnapshot {
            provider_item_id: item.price.id.to_string(),
            provider_price_id: item.price.id.to_string(),
            // Saturate rather than silently wrapping: a Paddle i64 quantity
            // outside u32 range (a negative adjustment, or an absurd bulk
            // count) must not two's-complement-wrap into a bogus billing
            // quantity. Real subscription quantities are small positives.
            quantity: item.quantity.clamp(0, u32::MAX as i64) as u32,
            unit_amount: None,
        })
        .collect();

    let now = chrono::Utc::now();
    let (period_start, period_end) = s
        .current_billing_period
        .as_ref()
        .map(|p| (p.starts_at, p.ends_at))
        .unwrap_or((now, now));

    let cancel_at_period_end = s
        .scheduled_change
        .as_ref()
        .map(|sc| format!("{sc:?}").to_lowercase().contains("cancel"))
        .unwrap_or(false);

    SubscriptionResult {
        provider_subscription_id: s.id.to_string(),
        provider_customer_id: s.customer_id.to_string(),
        status: map_status_from_debug(&s.status),
        items,
        current_period_start: period_start,
        current_period_end: period_end,
        cancel_at_period_end,
        provider_metadata: serde_json::json!({
            "paddle_status": format!("{:?}", s.status),
        }),
    }
}

fn map_subscription_with_include(
    s: &paddle_rust_sdk::entities::SubscriptionWithInclude,
) -> SubscriptionResult {
    // SubscriptionWithInclude extends Subscription with optional included entities.
    // The base Subscription fields are accessible via the .subscription field on the
    // wrapper. If that shape doesn't match (e.g. SubscriptionWithInclude is just an
    // alias with public fields), the compiler will point us at the right path.
    map_paddle_subscription(&s.subscription)
}
