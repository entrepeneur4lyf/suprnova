use crate::payments::{PaymentResult, SessionPayload, StartSessionRequest};
use async_trait::async_trait;

#[async_trait]
pub trait Checkout: Send + Sync {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload>;
}
