use thiserror::Error;

#[derive(Debug, Error)]
pub enum PaymentError {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("request validation failed: {0}")]
    Validation(String),

    #[error("operation not supported by this provider: {0}")]
    NotSupported(String),

    #[error("payment was declined: {reason}")]
    Declined { reason: String, decline_code: Option<String> },

    #[error("provider authentication failed: {0}")]
    Authentication(String),

    #[error("requested resource not found: {0}")]
    NotFound(String),

    #[error("webhook signature verification failed: {0}")]
    WebhookSignature(String),

    #[error("internal payments error: {0}")]
    Internal(String),
}

pub type PaymentResult<T> = Result<T, PaymentError>;
