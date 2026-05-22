//! Implementation of the `WebhookHandler` trait for `PaddleProvider`.
//!
//! Uses `Paddle::unmarshal` for signature verification — it handles the
//! `Paddle-Signature` header format (`ts=…,h1=…`) and HMAC validation with
//! timestamp-skew tolerance. No manual HMAC code needed.

use async_trait::async_trait;
use paddle_rust_sdk::{webhooks::MaximumVariance, Paddle};
use suprnova::payments::{
    NeutralEventKind, PaymentError, PaymentResult, WebhookContext, WebhookEvent, WebhookHandler,
};

use crate::{event_map::paddle_event_to_neutral, PaddleProvider};

#[async_trait]
impl WebhookHandler for PaddleProvider {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        let signature = ctx
            .headers
            .get("paddle-signature")
            .ok_or_else(|| {
                PaymentError::WebhookSignature("missing paddle-signature header".into())
            })?
            .to_str()
            .map_err(|_| PaymentError::WebhookSignature("non-ascii signature header".into()))?;

        let body_str = std::str::from_utf8(ctx.body).map_err(|_| {
            PaymentError::WebhookSignature("non-utf8 webhook body".into())
        })?;

        Paddle::unmarshal(
            body_str,
            self.webhook_key(),
            signature,
            MaximumVariance::default(),
        )
        .map_err(|e| PaymentError::WebhookSignature(format!("paddle signature verify: {e}")))?;

        Ok(())
    }

    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent> {
        let raw: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| PaymentError::Validation(format!("invalid paddle webhook body: {e}")))?;

        let provider_event_id = raw
            .get("event_id")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let provider_event_type = raw
            .get("event_type")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let neutral: Option<NeutralEventKind> = paddle_event_to_neutral(&provider_event_type);

        Ok(WebhookEvent {
            provider: "paddle".into(),
            provider_event_id,
            provider_event_type,
            neutral,
            raw_payload: raw,
        })
    }
}
