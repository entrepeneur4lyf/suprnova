//! Implementation of the `Checkout` trait for `PaddleProvider`.
//!
//! Paddle is checkout-driven: both `OneOff` and `Subscription` modes route
//! through `transaction_create`. Paddle dispatches on price-kind implicitly
//! (recurring prices → subscription, one-off prices → single charge). The
//! returned `transaction_id` is opened by the frontend via paddle.js with
//! the `client_token`.

use async_trait::async_trait;
use suprnova::payments::{
    Checkout, PaymentError, PaymentResult, SessionPayload, StartSessionRequest,
};

use crate::PaddleProvider;

#[async_trait]
impl Checkout for PaddleProvider {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload> {
        if req.price_refs.is_empty() {
            return Err(PaymentError::Validation(
                "start_session requires at least one price_ref".into(),
            ));
        }

        let mut builder = self.client().transaction_create();
        for price_ref in &req.price_refs {
            builder.append_catalog_item(price_ref.clone(), 1);
        }
        builder.customer_id(req.customer_ref.clone());

        let resp = builder
            .send()
            .await
            .map_err(|e| PaymentError::Provider(format!("paddle transaction_create: {e}")))?;

        Ok(SessionPayload::PaddleInline {
            transaction_id: resp.data.id.to_string(),
            customer_token: Some(req.customer_ref.clone()),
            client_token: self.client_token().to_string(),
        })
    }
}
