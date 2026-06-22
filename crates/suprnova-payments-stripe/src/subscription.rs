//! Implementation of the `Subscription` trait for `StripeProvider`.
//!
//! Maps Suprnova's provider-neutral subscription lifecycle onto Stripe's
//! `/v1/subscriptions` API.

use async_trait::async_trait;
use chrono::{DateTime, TimeZone, Utc};
use serde::Serialize;
use stripe_client_core::{RequestBuilder, StripeMethod};
use stripe_shared::{Subscription as StripeSubscription, SubscriptionStatus as StripeSubStatus};

use suprnova::payments::{
    Money, PaymentError, PaymentResult, SubscribeRequest, Subscription, SubscriptionItemSnapshot,
    SubscriptionResult, SubscriptionStatus, UpdateSubscriptionRequest,
};

use crate::StripeProvider;

#[derive(Serialize)]
struct CreateSubscriptionParams<'a> {
    customer: &'a str,
    #[serde(serialize_with = "serialize_items")]
    items: Vec<ItemParam<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    trial_period_days: Option<u32>,
}

#[derive(Serialize)]
struct ItemParam<'a> {
    price: &'a str,
    quantity: u32,
}

fn serialize_items<S>(items: &[ItemParam<'_>], s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(items.len()))?;
    for item in items {
        seq.serialize_element(item)?;
    }
    seq.end()
}

#[derive(Serialize)]
struct UpdateSubscriptionParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    cancel_at_period_end: Option<bool>,
}

#[derive(Serialize)]
struct CancelSubscriptionParams {
    invoice_now: bool,
    prorate: bool,
}

fn ts_to_dt(ts: i64) -> DateTime<Utc> {
    Utc.timestamp_opt(ts, 0).single().unwrap_or_else(Utc::now)
}

fn map_status(s: &StripeSubStatus) -> SubscriptionStatus {
    match s {
        StripeSubStatus::Trialing => SubscriptionStatus::Trialing,
        StripeSubStatus::Active => SubscriptionStatus::Active,
        StripeSubStatus::PastDue => SubscriptionStatus::PastDue,
        StripeSubStatus::Canceled => SubscriptionStatus::Canceled,
        StripeSubStatus::Incomplete | StripeSubStatus::IncompleteExpired => {
            SubscriptionStatus::Incomplete
        }
        StripeSubStatus::Paused => SubscriptionStatus::Paused,
        StripeSubStatus::Unpaid => SubscriptionStatus::PastDue,
        // SubscriptionStatus is #[non_exhaustive] — surface new states as Incomplete (safest
        // default; callers should treat as not-yet-billable until clarified by a webhook).
        _ => SubscriptionStatus::Incomplete,
    }
}

fn map_subscription(s: StripeSubscription) -> SubscriptionResult {
    let customer_id = match s.customer {
        stripe_types::Expandable::Id(id) => id.as_str().to_string(),
        stripe_types::Expandable::Object(obj) => obj.id.as_str().to_string(),
    };

    // In Stripe API 2023-08-16+ the billing-period timestamps moved from Subscription to
    // each SubscriptionItem. Multi-item subscriptions can theoretically have divergent
    // periods, but in practice all items share the parent's cycle. Take the first item's
    // period as the parent period.
    let (period_start, period_end) = s
        .items
        .data
        .first()
        .map(|item| (item.current_period_start, item.current_period_end))
        .unwrap_or((0, 0));

    let items: Vec<SubscriptionItemSnapshot> = s
        .items
        .data
        .iter()
        .map(|item| {
            let unit_amount = item.price.unit_amount.and_then(|amount| {
                let code = format!("{}", item.price.currency).to_uppercase();
                suprnova::payments::Currency::from_code(&code)
                    .map(|iso| Money::from_minor_units(amount, iso))
            });

            SubscriptionItemSnapshot {
                provider_item_id: item.id.as_str().to_string(),
                provider_price_id: item.price.id.as_str().to_string(),
                // Saturate a u64 quantity into u32 rather than truncating the
                // high bits; default to 1 when Stripe omits it (non-quantitative
                // prices).
                quantity: item
                    .quantity
                    .map(|q| u32::try_from(q).unwrap_or(u32::MAX))
                    .unwrap_or(1),
                unit_amount,
            }
        })
        .collect();

    SubscriptionResult {
        provider_subscription_id: s.id.as_str().to_string(),
        provider_customer_id: customer_id,
        status: map_status(&s.status),
        items,
        current_period_start: ts_to_dt(period_start),
        current_period_end: ts_to_dt(period_end),
        cancel_at_period_end: s.cancel_at_period_end,
        provider_metadata: serde_json::json!({
            "stripe_status": format!("{:?}", s.status),
        }),
    }
}

#[async_trait]
impl Subscription for StripeProvider {
    async fn subscribe(&self, req: SubscribeRequest) -> PaymentResult<SubscriptionResult> {
        if req.price_refs.is_empty() {
            return Err(PaymentError::Validation(
                "subscribe requires at least one price_ref".into(),
            ));
        }

        let items: Vec<ItemParam> = req
            .price_refs
            .iter()
            .map(|p| ItemParam {
                price: p,
                quantity: 1,
            })
            .collect();

        let params = CreateSubscriptionParams {
            customer: &req.customer_ref,
            items,
            trial_period_days: req.trial_days,
        };

        let sub: StripeSubscription = RequestBuilder::new(StripeMethod::Post, "/subscriptions")
            .form(&params)
            .customize::<StripeSubscription>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe subscriptions.create: {e}")))?;

        Ok(map_subscription(sub))
    }

    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult> {
        // v1 supports cancel_at_period_end only. Price-set replacement requires deleting +
        // creating items and is shaped differently per provider; return NotSupported honestly
        // rather than guess. Per advisor: this is the one place NotSupported is honest, not
        // deferral.
        if req.new_price_refs.is_some() {
            return Err(PaymentError::NotSupported(
                "Stripe price-set replacement on existing subscription not in v1. \
                 Cancel the subscription and create a new one with the new price set."
                    .into(),
            ));
        }

        let params = UpdateSubscriptionParams {
            cancel_at_period_end: req.cancel_at_period_end,
        };

        let path = format!("/subscriptions/{}", req.provider_subscription_id);
        let sub: StripeSubscription = RequestBuilder::new(StripeMethod::Post, &path)
            .form(&params)
            .customize::<StripeSubscription>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe subscriptions.update: {e}")))?;

        Ok(map_subscription(sub))
    }

    async fn cancel(
        &self,
        provider_subscription_id: &str,
        at_period_end: bool,
    ) -> PaymentResult<SubscriptionResult> {
        if at_period_end {
            let path = format!("/subscriptions/{provider_subscription_id}");
            let params = UpdateSubscriptionParams {
                cancel_at_period_end: Some(true),
            };
            let sub: StripeSubscription = RequestBuilder::new(StripeMethod::Post, &path)
                .form(&params)
                .customize::<StripeSubscription>()
                .send(self.client())
                .await
                .map_err(|e| {
                    PaymentError::Provider(format!("stripe subscriptions.update(cape): {e}"))
                })?;
            Ok(map_subscription(sub))
        } else {
            let path = format!("/subscriptions/{provider_subscription_id}");
            let params = CancelSubscriptionParams {
                invoice_now: false,
                prorate: false,
            };
            let sub: StripeSubscription = RequestBuilder::new(StripeMethod::Delete, &path)
                .form(&params)
                .customize::<StripeSubscription>()
                .send(self.client())
                .await
                .map_err(|e| PaymentError::Provider(format!("stripe subscriptions.cancel: {e}")))?;
            Ok(map_subscription(sub))
        }
    }

    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult> {
        let path = format!("/subscriptions/{provider_subscription_id}");
        let sub: StripeSubscription = RequestBuilder::new(StripeMethod::Get, &path)
            .customize::<StripeSubscription>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe subscriptions.retrieve: {e}")))?;

        Ok(map_subscription(sub))
    }
}
