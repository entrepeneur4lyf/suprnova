//! Error type shared across every payments trait, DTO, and provider adapter.

use thiserror::Error;

/// Errors returned by the payments subsystem.
///
/// Provider adapters translate their SDK error shapes into this enum so
/// application code can match on a single variant set regardless of
/// which rail it is talking to.
#[derive(Debug, Error)]
pub enum PaymentError {
    /// Provider-side failure with no more specific classification (5xx,
    /// unexpected payload shape, transport error wrapping, etc.).
    #[error("provider error: {0}")]
    Provider(String),

    /// Caller-supplied request failed pre-flight validation. The message
    /// names the offending field or constraint.
    #[error("request validation failed: {0}")]
    Validation(String),

    /// The provider does not implement the requested operation
    /// (e.g. server-side capture on a Merchant-of-Record).
    #[error("operation not supported by this provider: {0}")]
    NotSupported(String),

    /// The payment was declined by the issuer or risk system.
    #[error("payment was declined: {reason}")]
    Declined {
        /// Human-readable reason as reported by the provider.
        reason: String,
        /// Provider-specific decline code (e.g. Stripe's
        /// `insufficient_funds`), when the provider supplies one.
        decline_code: Option<String>,
    },

    /// API key / signing key / bearer token rejected by the provider.
    #[error("provider authentication failed: {0}")]
    Authentication(String),

    /// The requested resource (customer, payment method, transaction,
    /// subscription, etc.) does not exist on the provider.
    #[error("requested resource not found: {0}")]
    NotFound(String),

    /// Inbound webhook signature failed verification — payload was either
    /// forged or tampered with in transit.
    #[error("webhook signature verification failed: {0}")]
    WebhookSignature(String),

    /// Phone number could not be parsed as a valid E.164 value. See
    /// [`super::PhoneNumber::new`].
    #[error("invalid phone number: {0}")]
    InvalidPhoneNumber(String),

    /// Country code is not a valid ISO 3166-1 alpha-2 value. See
    /// [`super::CountryCode::new`].
    #[error("invalid country code: {0}")]
    InvalidCountryCode(String),

    /// Internal framework error — surfacing a bug, not a recoverable
    /// caller mistake. Operators should treat this as a paging condition.
    #[error("internal payments error: {0}")]
    Internal(String),
}

/// Convenience alias for `Result<T, PaymentError>`.
pub type PaymentResult<T> = Result<T, PaymentError>;
