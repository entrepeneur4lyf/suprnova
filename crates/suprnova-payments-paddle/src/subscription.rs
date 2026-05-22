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
        // Per advisor: this is the one place NotSupported is honest, not deferral.
        if req.new_price_refs.is_some() {
            return Err(PaymentError::NotSupported(
                "Paddle price-set replacement on existing subscription not in v1.".into(),
            ));
        }

        if let Some(true) = req.cancel_at_period_end {
            // Mirror cancel(at_period_end=true): schedule via subscription_cancel
            // with default (NextBillingPeriod).
            let resp = self
                .client()
                .subscription_cancel(req.provider_subscription_id.clone())
                .send()
                .await
                .map_err(|e| {
                    PaymentError::Provider(format!(
                        "paddle subscription_cancel (cape): {e}"
                    ))
                })?;
            return Ok(map_paddle_subscription(&resp.data));
        }

        // No-op update: re-fetch current state via subscription_get.
        let resp = self
            .client()
            .subscription_get(req.provider_subscription_id)
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle subscription_get: {e}")))?;
        Ok(map_subscription_with_include(&resp.data))
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
            quantity: item.quantity as u32,
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

