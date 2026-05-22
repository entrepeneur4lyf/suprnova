use crate::payments::{PaymentResult, WebhookContext, WebhookEvent};
use async_trait::async_trait;

#[async_trait]
pub trait WebhookHandler: Send + Sync {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()>;
    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent>;
}
