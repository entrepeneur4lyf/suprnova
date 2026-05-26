//! Implementation of the `Checkout` trait for `StripeProvider`.
//!
//! For `SessionMode::OneOff` we create a PaymentIntent and return its
//! `client_secret` + publishable key so the frontend can mount Stripe Elements.
//! For `SessionMode::Subscription` we create a hosted Checkout Session and
//! return its url so the frontend redirects.

use async_trait::async_trait;
use serde::Serialize;
use stripe_client_core::{RequestBuilder, StripeMethod};
use stripe_shared::{CheckoutSession, PaymentIntent};

use suprnova::payments::{
    Checkout, PaymentError, PaymentResult, SessionMode, SessionPayload, StartSessionRequest,
};

use crate::StripeProvider;

#[derive(Serialize)]
struct CreatePaymentIntentParams<'a> {
    amount: i64,
    currency: &'a str,
    customer: &'a str,
    #[serde(rename = "automatic_payment_methods[enabled]")]
    automatic_payment_methods_enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
}

#[derive(Serialize)]
struct CreateCheckoutSessionParams<'a> {
    mode: &'static str,
    customer: &'a str,
    success_url: &'a str,
    cancel_url: &'a str,
    /// Stripe expects line_items as form-encoded array fields, e.g.
    /// `line_items[0][price]=price_xxx&line_items[0][quantity]=1`. We
    /// serialize a single line-item per price_ref via a custom serializer.
    #[serde(serialize_with = "serialize_line_items")]
    line_items: Vec<LineItem<'a>>,
}

#[derive(Serialize)]
struct LineItem<'a> {
    price: &'a str,
    quantity: u32,
}

fn serialize_line_items<S>(items: &[LineItem<'_>], s: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    // Stripe wire format for arrays: line_items[0][price]=...&line_items[0][quantity]=...
    // serde_urlencoded handles this naturally via tuple-serialization, but the array-index
    // syntax requires a flat string. We emit a single comma-separated price list as
    // line_items[0][price] for v1 — most subscriptions are single-price.
    use serde::ser::SerializeSeq;
    let mut seq = s.serialize_seq(Some(items.len()))?;
    for item in items {
        seq.serialize_element(item)?;
    }
    seq.end()
}

#[async_trait]
impl Checkout for StripeProvider {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload> {
        match req.mode {
            SessionMode::OneOff => {
                let amount = req.amount_hint.ok_or_else(|| {
                    PaymentError::Validation("OneOff mode requires amount_hint".into())
                })?;
                let currency = amount.currency().code().to_lowercase();

                let params = CreatePaymentIntentParams {
                    amount: amount.minor_units(),
                    currency: &currency,
                    customer: &req.customer_ref,
                    automatic_payment_methods_enabled: true,
                    description: None,
                };

                let intent: PaymentIntent =
                    RequestBuilder::new(StripeMethod::Post, "/payment_intents")
                        .form(&params)
                        .customize::<PaymentIntent>()
                        .send(self.client())
                        .await
                        .map_err(|e| {
                            PaymentError::Provider(format!("stripe payment_intents.create: {e}"))
                        })?;

                let client_secret = intent.client_secret.ok_or_else(|| {
                    PaymentError::Provider("PaymentIntent missing client_secret on create".into())
                })?;

                Ok(SessionPayload::StripeElements {
                    client_secret,
                    publishable_key: self.publishable_key().to_string(),
                    provider_session_id: intent.id.as_str().to_string(),
                })
            }
            SessionMode::Subscription => {
                let line_items: Vec<LineItem> = req
                    .price_refs
                    .iter()
                    .map(|p| LineItem {
                        price: p,
                        quantity: 1,
                    })
                    .collect();

                if line_items.is_empty() {
                    return Err(PaymentError::Validation(
                        "Subscription mode requires at least one price_ref".into(),
                    ));
                }

                let params = CreateCheckoutSessionParams {
                    mode: "subscription",
                    customer: &req.customer_ref,
                    success_url: &req.success_return_url,
                    cancel_url: &req.cancel_return_url,
                    line_items,
                };

                let session: CheckoutSession =
                    RequestBuilder::new(StripeMethod::Post, "/checkout/sessions")
                        .form(&params)
                        .customize::<CheckoutSession>()
                        .send(self.client())
                        .await
                        .map_err(|e| {
                            PaymentError::Provider(format!("stripe checkout.sessions.create: {e}"))
                        })?;

                let url = session.url.ok_or_else(|| {
                    PaymentError::Provider("CheckoutSession missing url on create".into())
                })?;

                Ok(SessionPayload::StripeCheckoutRedirect {
                    url,
                    provider_session_id: session.id.as_str().to_string(),
                })
            }
        }
    }
}
