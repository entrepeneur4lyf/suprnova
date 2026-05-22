use std::sync::Arc;
use suprnova::payments::*;

/// Lightweight wrapper demonstrating the per-user billing ergonomic.
///
/// In a real app this would be a trait extension on the `User` model, but
/// keeping it as a free-standing struct here lets the dogfood test stand
/// alone without coupling to the User model's evolving shape.
pub struct BillableUser {
    pub user_id: String,
    pub email: String,
}

impl BillableUser {
    /// Create a customer with the provider, then open a checkout session for
    /// the given price. Returns the flow-tagged SessionPayload — the frontend
    /// dispatches on `flow` to render the right widget.
    pub async fn start_subscription(
        &self,
        provider: Arc<dyn PaymentProvider>,
        price_ref: &str,
        success_url: &str,
        cancel_url: &str,
    ) -> PaymentResult<SessionPayload> {
        let customer = provider
            .create_customer(CreateCustomerRequest {
                user_id: self.user_id.clone(),
                email: self.email.clone(),
                name: None,
                metadata: None,
            })
            .await?;

        provider
            .start_session(StartSessionRequest {
                mode: SessionMode::Subscription,
                customer_ref: customer.provider_customer_id,
                price_refs: vec![price_ref.into()],
                success_return_url: success_url.into(),
                cancel_return_url: cancel_url.into(),
                amount_hint: None,
                idempotency_key: None,
                metadata: None,
            })
            .await
    }
}
