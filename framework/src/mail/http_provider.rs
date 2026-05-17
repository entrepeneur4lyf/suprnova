//! Shared helpers for HTTP-based mail providers (Postmark, SES, SendGrid, …).

use crate::error::FrameworkError;
use reqwest::Client;
use std::sync::OnceLock;

/// One shared `reqwest::Client` across all HTTP-mail transports.
/// Connection-pooled, rustls, no PII headers.
pub(crate) fn shared_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("suprnova-mail/", env!("CARGO_PKG_VERSION")))
            .build()
            .expect("reqwest client builder")
    })
}

pub(crate) fn err(provider: &'static str, status: u16, body: String) -> FrameworkError {
    FrameworkError::internal(format!("{provider} HTTP {status}: {body}"))
}
