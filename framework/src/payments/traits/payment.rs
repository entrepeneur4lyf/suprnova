use crate::payments::{
    ChargeRequest, ChargeResult, PaymentResult, PaymentStatus, RefundRequest, RefundResult,
};
use async_trait::async_trait;

/// Optional trait for providers that expose server-side capture against a stored payment method.
/// Paddle and other Merchant-of-Record providers do NOT implement this.
#[async_trait]
pub trait Payment: Send + Sync {
    async fn charge(&self, req: ChargeRequest) -> PaymentResult<ChargeResult>;
    async fn capture(&self, provider_transaction_id: &str) -> PaymentResult<ChargeResult>;
    async fn refund(&self, req: RefundRequest) -> PaymentResult<RefundResult>;
    async fn void(&self, provider_transaction_id: &str) -> PaymentResult<()>;
    async fn status(&self, provider_transaction_id: &str) -> PaymentResult<PaymentStatus>;
}
