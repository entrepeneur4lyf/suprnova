//! In-memory mock payment provider for tests.
//!
//! [`MockPaymentProvider`] implements all four universal traits — [`Checkout`],
//! [`Subscription`], [`CustomerStore`], and [`WebhookHandler`] — entirely in memory,
//! with no external dependencies. It deliberately does NOT implement [`Payment`]
//! (server-capture) to exercise the Paddle-style "optional Payment" invariant: callers
//! that query `provider.as_payment()` will receive `None`.
//!
//! # Usage
//!
//! ```rust,ignore
//! use suprnova::payments::*;
//!
//! let provider = MockPaymentProvider::new();
//! let cus = provider.create_customer(CreateCustomerRequest {
//!     user_id: "user_1".into(),
//!     email: "test@example.com".into(),
//!     name: None,
//!     metadata: None,
//! }).await.unwrap();
//! ```
//!
//! See `framework/tests/payments_mock_discriminator.rs` for the full E2E flow.

use crate::payments::{
    Checkout, CreateCustomerRequest, CustomerRef, CustomerSnapshot, CustomerStore,
    NeutralEventKind, PayloadIds, PaymentError, PaymentProvider, PaymentResult, PaymentSnapshot,
    SessionPayload, StartSessionRequest, SubscribeRequest, Subscription, SubscriptionItemSnapshot,
    SubscriptionResult, SubscriptionStatus, UpdateCustomerRequest, UpdateSubscriptionRequest,
    WebhookContext, WebhookEvent, WebhookHandler,
};
use async_trait::async_trait;
use chrono::Utc;
use serde_json::json;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

/// In-memory implementation of all four universal payment traits.
///
/// Thread-safe: internal state is wrapped in `Arc<tokio::sync::RwLock<_>>`.
/// Instances can be cloned or wrapped in `Arc` and shared across async tasks.
///
/// Uses tokio's async `RwLock` rather than `std::sync::RwLock` so that the lock
/// surface is poison-immune (Domain 19 audit D19-B): a panic inside one trait
/// method can no longer poison the shared store and cascade `PaymentError`s
/// through every subsequent call.
#[derive(Default)]
pub struct MockPaymentProvider {
    customers: Arc<RwLock<HashMap<String, CustomerRef>>>,
    subscriptions: Arc<RwLock<HashMap<String, SubscriptionResult>>>,
    sequence: Arc<RwLock<u64>>,
}

impl MockPaymentProvider {
    /// Create a new, empty mock provider.
    pub fn new() -> Self {
        Self::default()
    }

    async fn next_id(&self, prefix: &str) -> String {
        let mut seq = self.sequence.write().await;
        *seq += 1;
        format!("{prefix}_mock_{}", *seq)
    }
}

impl PaymentProvider for MockPaymentProvider {
    fn name(&self) -> &'static str {
        "mock"
    }
    // `as_payment()` intentionally uses the default `None` — no `Payment` impl.
}

#[async_trait]
impl Checkout for MockPaymentProvider {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload> {
        let session_id = self.next_id("ses").await;
        Ok(SessionPayload::Redirect {
            url: format!("https://mock.example/{}/{}", req.customer_ref, session_id),
            provider_session_id: session_id,
        })
    }
}

#[async_trait]
impl Subscription for MockPaymentProvider {
    async fn subscribe(&self, req: SubscribeRequest) -> PaymentResult<SubscriptionResult> {
        let id = self.next_id("sub").await;
        let now = Utc::now();
        let result = SubscriptionResult {
            provider_subscription_id: id.clone(),
            provider_customer_id: req.customer_ref.clone(),
            status: SubscriptionStatus::Active,
            items: req
                .price_refs
                .iter()
                .map(|price_ref| SubscriptionItemSnapshot {
                    provider_item_id: format!("{id}_item_{price_ref}"),
                    provider_price_id: price_ref.clone(),
                    quantity: 1,
                    unit_amount: None,
                })
                .collect(),
            current_period_start: now,
            current_period_end: now + chrono::Duration::days(30),
            cancel_at_period_end: false,
            provider_metadata: json!({}),
        };
        self.subscriptions.write().await.insert(id, result.clone());
        Ok(result)
    }

    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult> {
        let mut store = self.subscriptions.write().await;
        let sub = store
            .get_mut(&req.provider_subscription_id)
            .ok_or_else(|| PaymentError::NotFound(req.provider_subscription_id.clone()))?;
        if let Some(c) = req.cancel_at_period_end {
            sub.cancel_at_period_end = c;
        }
        if let Some(prices) = req.new_price_refs {
            let sub_id = sub.provider_subscription_id.clone();
            sub.items = prices
                .iter()
                .map(|price_ref| SubscriptionItemSnapshot {
                    provider_item_id: format!("{sub_id}_item_{price_ref}"),
                    provider_price_id: price_ref.clone(),
                    quantity: 1,
                    unit_amount: None,
                })
                .collect();
        }
        Ok(sub.clone())
    }

    async fn cancel(
        &self,
        provider_subscription_id: &str,
        at_period_end: bool,
    ) -> PaymentResult<SubscriptionResult> {
        let mut store = self.subscriptions.write().await;
        let sub = store
            .get_mut(provider_subscription_id)
            .ok_or_else(|| PaymentError::NotFound(provider_subscription_id.to_string()))?;
        if at_period_end {
            sub.cancel_at_period_end = true;
        } else {
            sub.status = SubscriptionStatus::Canceled;
        }
        Ok(sub.clone())
    }

    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult> {
        self.subscriptions
            .read()
            .await
            .get(provider_subscription_id)
            .cloned()
            .ok_or_else(|| PaymentError::NotFound(provider_subscription_id.to_string()))
    }
}

#[async_trait]
impl CustomerStore for MockPaymentProvider {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        let id = self.next_id("cus").await;
        let cr = CustomerRef {
            provider_customer_id: id.clone(),
            user_id: Some(req.user_id),
            email: req.email,
            provider_metadata: req.metadata.unwrap_or(json!({})),
        };
        self.customers.write().await.insert(id, cr.clone());
        Ok(cr)
    }

    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        let mut store = self.customers.write().await;
        let cr = store
            .get_mut(&req.provider_customer_id)
            .ok_or_else(|| PaymentError::NotFound(req.provider_customer_id.clone()))?;
        if let Some(e) = req.email {
            cr.email = e;
        }
        if let Some(m) = req.metadata {
            cr.provider_metadata = m;
        }
        Ok(cr.clone())
    }

    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef> {
        self.customers
            .read()
            .await
            .get(provider_customer_id)
            .cloned()
            .ok_or_else(|| PaymentError::NotFound(provider_customer_id.to_string()))
    }

    async fn delete_customer(&self, provider_customer_id: &str) -> PaymentResult<()> {
        self.customers
            .write()
            .await
            .remove(provider_customer_id)
            .map(|_| ())
            .ok_or_else(|| PaymentError::NotFound(provider_customer_id.to_string()))
    }
}

#[async_trait]
impl WebhookHandler for MockPaymentProvider {
    fn verify(&self, _ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        Ok(())
    }

    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent> {
        let raw: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| PaymentError::Validation(format!("invalid mock webhook body: {e}")))?;
        let provider_event_type = raw
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("mock.event")
            .to_string();
        let neutral = match provider_event_type.as_str() {
            "subscription.created" => Some(NeutralEventKind::SubscriptionCreated),
            "subscription.updated" => Some(NeutralEventKind::SubscriptionUpdated),
            "subscription.canceled" => Some(NeutralEventKind::SubscriptionCanceled),
            "payment.succeeded" => Some(NeutralEventKind::PaymentSucceeded),
            "payment.failed" => Some(NeutralEventKind::PaymentFailed),
            "payment.refunded" => Some(NeutralEventKind::PaymentRefunded),
            "payment.disputed" => Some(NeutralEventKind::PaymentDisputed),
            "invoice.paid" => Some(NeutralEventKind::InvoicePaid),
            "invoice.failed" => Some(NeutralEventKind::InvoiceFailed),
            "customer.created" => Some(NeutralEventKind::CustomerCreated),
            "customer.updated" => Some(NeutralEventKind::CustomerUpdated),
            _ => None,
        };
        Ok(WebhookEvent {
            provider: "mock".into(),
            provider_event_id: raw
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("evt_mock")
                .to_string(),
            provider_event_type,
            neutral,
            raw_payload: raw,
        })
    }

    /// Mock follows Stripe's envelope convention: `data.object.*` carries the
    /// entity, with `id`, `customer`, and `subscription` as canonical pointers.
    fn extract_payload_ids(&self, event: &WebhookEvent) -> PayloadIds {
        let obj = match event.raw_payload.pointer("/data/object") {
            Some(o) => o,
            None => return PayloadIds::default(),
        };
        let mut ids = PayloadIds::default();
        match event.neutral {
            Some(
                NeutralEventKind::SubscriptionCreated
                | NeutralEventKind::SubscriptionUpdated
                | NeutralEventKind::SubscriptionCanceled,
            ) => {
                ids.subscription_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = obj
                    .get("customer")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            Some(NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated) => {
                ids.customer_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
            }
            Some(
                NeutralEventKind::PaymentSucceeded
                | NeutralEventKind::PaymentFailed
                | NeutralEventKind::PaymentRefunded
                | NeutralEventKind::PaymentDisputed
                | NeutralEventKind::InvoicePaid
                | NeutralEventKind::InvoiceFailed,
            ) => {
                ids.transaction_id = obj.get("id").and_then(|v| v.as_str()).map(String::from);
                ids.customer_id = obj
                    .get("customer")
                    .and_then(|v| v.as_str())
                    .map(String::from);
                ids.subscription_id = obj
                    .get("subscription")
                    .and_then(|v| v.as_str())
                    .map(String::from);
            }
            None => {}
        }
        ids
    }

    fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot> {
        let obj = event.raw_payload.pointer("/data/object")?;
        let provider_transaction_id = obj.get("id")?.as_str()?.to_string();
        let provider_customer_id = obj
            .get("customer")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let provider_subscription_id = obj
            .get("subscription")
            .and_then(|v| v.as_str())
            .map(String::from);
        let amount_total_minor = obj.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
        let amount_tax_minor = obj.get("tax").and_then(|v| v.as_i64()).unwrap_or(0);
        let currency = obj
            .get("currency")
            .and_then(|v| v.as_str())
            .unwrap_or("USD")
            .to_uppercase();
        let status = match event.neutral? {
            NeutralEventKind::PaymentSucceeded | NeutralEventKind::InvoicePaid => "succeeded",
            NeutralEventKind::PaymentFailed | NeutralEventKind::InvoiceFailed => "failed",
            NeutralEventKind::PaymentRefunded => "refunded",
            NeutralEventKind::PaymentDisputed => "disputed",
            _ => return None,
        }
        .to_string();
        let paid_at = obj
            .get("paid_at")
            .and_then(|v| v.as_str())
            .and_then(|s| chrono::DateTime::parse_from_rfc3339(s).ok())
            .map(|dt| dt.with_timezone(&chrono::Utc));
        Some(PaymentSnapshot {
            provider_transaction_id,
            provider_customer_id,
            provider_subscription_id,
            amount_total_minor,
            amount_tax_minor,
            currency,
            status,
            paid_at,
            provider_metadata: obj.clone(),
        })
    }

    fn extract_customer_snapshot(&self, event: &WebhookEvent) -> Option<CustomerSnapshot> {
        match event.neutral? {
            NeutralEventKind::CustomerCreated | NeutralEventKind::CustomerUpdated => {
                let obj = event.raw_payload.pointer("/data/object")?;
                let provider_customer_id = obj.get("id")?.as_str()?.to_string();
                let email = obj
                    .get("email")
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string());
                Some(CustomerSnapshot {
                    provider_customer_id,
                    email,
                    provider_metadata: obj.clone(),
                })
            }
            _ => None,
        }
    }
}
