pub mod checkout;
pub mod customer;
pub mod payment;
pub mod subscription;
pub mod webhook;

pub use checkout::Checkout;
pub use customer::CustomerStore;
pub use payment::Payment;
pub use subscription::Subscription;
pub use webhook::{CustomerSnapshot, PayloadIds, PaymentSnapshot, WebhookHandler};

/// Umbrella trait every provider MUST implement. `Payment` is queried separately —
/// providers that don't expose server-capture omit that impl.
pub trait PaymentProvider: Checkout + Subscription + CustomerStore + WebhookHandler {
    /// Stable kebab-case identifier — e.g. "stripe", "paddle". Used for webhook routing.
    fn name(&self) -> &'static str;

    /// Returns `Some` if this provider also implements `Payment` (server-capture).
    /// Default impl returns `None`; implementers override when their type also impls `Payment`.
    fn as_payment(&self) -> Option<&dyn Payment> {
        None
    }
}
