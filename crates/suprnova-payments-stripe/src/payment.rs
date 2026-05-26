//! Implementation of the `Payment` trait for `StripeProvider`.
//!
//! Maps Suprnova's provider-neutral charge/capture/refund/void/status surface
//! onto Stripe PaymentIntents (`/v1/payment_intents`).

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;
use stripe_client_core::{RequestBuilder, StripeMethod};
use stripe_shared::{PaymentIntent, PaymentIntentStatus, Refund};

use suprnova::payments::traits::Payment;
use suprnova::payments::{
    ChargeRequest, ChargeResult, Money, PaymentError, PaymentResult, PaymentStatus, RefundRequest,
    RefundResult,
};

use crate::StripeProvider;

// ---------------------------------------------------------------------------
// Param structs
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct CreatePaymentIntentParams<'a> {
    amount: i64,
    currency: &'a str,
    customer: &'a str,
    payment_method: &'a str,
    confirm: bool,
    capture_method: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<&'a str>,
}

#[derive(Serialize)]
struct CreateRefundParams<'a> {
    payment_intent: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    amount: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<&'a str>,
}

// ---------------------------------------------------------------------------
// Currency helpers
// ---------------------------------------------------------------------------

/// Convert `suprnova::payments::Money` currency to a lowercase Stripe currency code.
fn money_to_stripe_currency(money: &Money) -> String {
    money.currency().code().to_lowercase()
}

/// Convert a `stripe_types::Currency` to `suprnova::payments::Money` given a minor-unit amount.
fn stripe_currency_to_money(
    amount: i64,
    currency: stripe_types::Currency,
) -> Result<Money, PaymentError> {
    let code = format!("{}", currency).to_uppercase();
    let iso = suprnova::payments::Currency::from_code(&code)
        .ok_or_else(|| PaymentError::Provider(format!("unknown Stripe currency: {code}")))?;
    Ok(Money::from_minor_units(amount, iso))
}

// ---------------------------------------------------------------------------
// Status mapping
// ---------------------------------------------------------------------------

fn map_pi_status(status: &PaymentIntentStatus) -> PaymentStatus {
    match status {
        PaymentIntentStatus::Succeeded => PaymentStatus::Succeeded,
        PaymentIntentStatus::Processing => PaymentStatus::Pending,
        PaymentIntentStatus::RequiresCapture => PaymentStatus::Pending,
        PaymentIntentStatus::RequiresAction
        | PaymentIntentStatus::RequiresConfirmation
        | PaymentIntentStatus::RequiresPaymentMethod => PaymentStatus::Pending,
        PaymentIntentStatus::Canceled => PaymentStatus::Canceled,
        // PaymentIntentStatus is #[non_exhaustive]; new Stripe statuses surface as Failed.
        _ => PaymentStatus::Failed,
    }
}

// ---------------------------------------------------------------------------
// Response mapping
// ---------------------------------------------------------------------------

fn pi_to_charge_result(
    intent: PaymentIntent,
    publishable_key: &str,
) -> Result<ChargeResult, PaymentError> {
    let id = intent.id.as_str().to_string();
    let amount = stripe_currency_to_money(intent.amount, intent.currency)?;
    let meta: Value = serde_json::json!({
        "stripe_status": intent.status.as_str(),
        "stripe_capture_method": intent.capture_method.as_str(),
    });

    match &intent.status {
        PaymentIntentStatus::Succeeded => Ok(ChargeResult::Completed {
            provider_transaction_id: id,
            amount,
            status: PaymentStatus::Succeeded,
            provider_metadata: meta,
        }),
        PaymentIntentStatus::RequiresCapture => Ok(ChargeResult::Completed {
            provider_transaction_id: id,
            amount,
            status: PaymentStatus::Pending,
            provider_metadata: meta,
        }),
        PaymentIntentStatus::RequiresAction => {
            // Try to surface the redirect URL from next_action.
            let redirect_url = intent
                .next_action
                .as_ref()
                .and_then(|na| na.redirect_to_url.as_ref())
                .and_then(|r| r.url.clone());

            Ok(ChargeResult::RequiresClientAction {
                provider_transaction_id: id,
                action_kind: "stripe_3ds".to_string(),
                client_secret: intent.client_secret,
                publishable_key: Some(publishable_key.to_string()),
            })
            .map(|mut r| {
                // If there's a redirect URL, prefer RedirectRequired.
                if let Some(url) = redirect_url {
                    r = ChargeResult::RedirectRequired {
                        provider_transaction_id: match &r {
                            ChargeResult::RequiresClientAction {
                                provider_transaction_id,
                                ..
                            } => provider_transaction_id.clone(),
                            _ => unreachable!(),
                        },
                        url,
                        return_to: None,
                    };
                }
                r
            })
        }
        PaymentIntentStatus::Processing
        | PaymentIntentStatus::RequiresConfirmation
        | PaymentIntentStatus::RequiresPaymentMethod => Ok(ChargeResult::Completed {
            provider_transaction_id: id,
            amount,
            status: PaymentStatus::Pending,
            provider_metadata: meta,
        }),
        PaymentIntentStatus::Canceled => Ok(ChargeResult::Completed {
            provider_transaction_id: id,
            amount,
            status: PaymentStatus::Canceled,
            provider_metadata: meta,
        }),
        // PaymentIntentStatus is #[non_exhaustive] — surface unrecognized statuses honestly.
        other => Err(PaymentError::Provider(format!(
            "PaymentIntent has unrecognized status: {}",
            other.as_str()
        ))),
    }
}

fn refund_to_result(refund: Refund) -> Result<RefundResult, PaymentError> {
    let pi_id = refund
        .payment_intent
        .map(|exp| match exp {
            stripe_types::Expandable::Id(id) => id.as_str().to_string(),
            stripe_types::Expandable::Object(pi) => pi.id.as_str().to_string(),
        })
        .ok_or_else(|| {
            PaymentError::Provider("Stripe refund missing payment_intent field".to_string())
        })?;

    let amount = stripe_currency_to_money(refund.amount, refund.currency)?;

    // RefundReason / RefundStatus from stripe-shared are #[non_exhaustive] enums without
    // serde::Serialize impls — debug-format them into JSON strings for the audit metadata.
    Ok(RefundResult {
        provider_refund_id: refund.id.as_str().to_string(),
        provider_transaction_id: pi_id,
        amount,
        provider_metadata: serde_json::json!({
            "stripe_status": refund.status.as_ref().map(|s| format!("{s:?}")),
            "stripe_reason": refund.reason.as_ref().map(|r| format!("{r:?}")),
        }),
    })
}

// ---------------------------------------------------------------------------
// Trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Payment for StripeProvider {
    /// Create and confirm a PaymentIntent against the customer's saved payment method.
    ///
    /// `capture_method` is set to `"manual"` so that the caller can capture later
    /// (via [`Payment::capture`]) or void (via [`Payment::void`]) if needed.
    /// For immediate capture set `capture_method` to `"automatic"` — but that
    /// pattern should go through `Checkout::start_session` instead.
    async fn charge(&self, req: ChargeRequest) -> PaymentResult<ChargeResult> {
        let currency = money_to_stripe_currency(&req.amount);
        let params = CreatePaymentIntentParams {
            amount: req.amount.minor_units(),
            currency: &currency,
            customer: &req.customer_ref,
            payment_method: &req.payment_method_ref,
            confirm: true,
            capture_method: "manual",
            description: req.description.as_deref(),
            idempotency_key: req.idempotency_key.as_deref(),
        };

        let intent: PaymentIntent = RequestBuilder::new(StripeMethod::Post, "/payment_intents")
            .form(&params)
            .customize::<PaymentIntent>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe payment_intents.create: {e}")))?;

        pi_to_charge_result(intent, self.publishable_key())
    }

    /// Capture a previously-authorised (requires_capture) PaymentIntent.
    async fn capture(&self, provider_transaction_id: &str) -> PaymentResult<ChargeResult> {
        let path = format!("/payment_intents/{provider_transaction_id}/capture");
        let intent: PaymentIntent = RequestBuilder::new(StripeMethod::Post, &path)
            .customize::<PaymentIntent>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe payment_intents.capture: {e}")))?;

        pi_to_charge_result(intent, self.publishable_key())
    }

    /// Refund a PaymentIntent fully or partially.
    async fn refund(&self, req: RefundRequest) -> PaymentResult<RefundResult> {
        let amount_minor = req.amount.as_ref().map(|m| m.minor_units());
        let params = CreateRefundParams {
            payment_intent: &req.provider_transaction_id,
            amount: amount_minor,
            reason: req.reason.as_deref(),
        };

        let refund: Refund = RequestBuilder::new(StripeMethod::Post, "/refunds")
            .form(&params)
            .customize::<Refund>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe refunds.create: {e}")))?;

        refund_to_result(refund)
    }

    /// Cancel a PaymentIntent that has not yet been captured.
    ///
    /// Returns `PaymentError::Validation` if the intent is already captured
    /// (Stripe will reject the cancel call in that state; use `refund` instead).
    async fn void(&self, provider_transaction_id: &str) -> PaymentResult<()> {
        let path = format!("/payment_intents/{provider_transaction_id}/cancel");

        let intent: PaymentIntent = RequestBuilder::new(StripeMethod::Post, &path)
            .customize::<PaymentIntent>()
            .send(self.client())
            .await
            .map_err(|e| {
                let msg = format!("{e}");
                // Stripe returns a 409 for intents already captured with a message
                // containing "already succeeded" — surface that as Validation.
                if msg.contains("already succeeded") || msg.contains("You cannot cancel") {
                    PaymentError::Validation(format!(
                        "cannot void payment_intent {provider_transaction_id}: {msg}"
                    ))
                } else {
                    PaymentError::Provider(format!("stripe payment_intents.cancel: {msg}"))
                }
            })?;

        match intent.status {
            PaymentIntentStatus::Canceled => Ok(()),
            ref other => Err(PaymentError::Provider(format!(
                "unexpected status after cancel: {}",
                other.as_str()
            ))),
        }
    }

    /// Retrieve the current status of a PaymentIntent.
    async fn status(&self, provider_transaction_id: &str) -> PaymentResult<PaymentStatus> {
        let path = format!("/payment_intents/{provider_transaction_id}");

        let intent: PaymentIntent = RequestBuilder::new(StripeMethod::Get, &path)
            .customize::<PaymentIntent>()
            .send(self.client())
            .await
            .map_err(|e| PaymentError::Provider(format!("stripe payment_intents.retrieve: {e}")))?;

        Ok(map_pi_status(&intent.status))
    }
}
