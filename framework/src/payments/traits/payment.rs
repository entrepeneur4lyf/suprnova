//! Server-capture trait for gateway-style payment providers.

use crate::payments::{
    ChargeRequest, ChargeResult, PaymentResult, PaymentStatus, RefundRequest, RefundResult,
};
use async_trait::async_trait;

/// Optional trait for providers that expose server-side capture against a stored payment method.
/// Paddle and other Merchant-of-Record providers do NOT implement this.
#[async_trait]
pub trait Payment: Send + Sync {
    /// Charge a stored payment method. May complete server-side, redirect
    /// for SCA, or hand off to a client-side action — distinguished by
    /// the [`ChargeResult`] variant.
    async fn charge(&self, req: ChargeRequest) -> PaymentResult<ChargeResult>;
    /// Capture funds previously authorized via a separate-capture flow.
    async fn capture(&self, provider_transaction_id: &str) -> PaymentResult<ChargeResult>;
    /// Refund a settled charge, in full or in part.
    async fn refund(&self, req: RefundRequest) -> PaymentResult<RefundResult>;
    /// Void an authorization before it is captured. After capture, use
    /// [`Self::refund`] instead.
    async fn void(&self, provider_transaction_id: &str) -> PaymentResult<()>;
    /// Fetch the current lifecycle status of a provider-side transaction.
    async fn status(&self, provider_transaction_id: &str) -> PaymentResult<PaymentStatus>;
}
