//! Shared helpers for HTTP-based mail providers (Postmark, SES, SendGrid, …).

use crate::error::FrameworkError;
use reqwest::Client;
use std::sync::OnceLock;
use std::time::Duration;

/// Per-request total timeout for HTTP mail providers. Matches the
/// `suprnova-web-push` `DEFAULT_REQUEST_TIMEOUT` so the entire framework
/// uses one upper bound on outbound provider calls.
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// Connect-only timeout for HTTP mail providers. A separate, shorter
/// budget so a black-holed TLS handshake fails fast rather than burning
/// the entire request budget.
const DEFAULT_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// One shared `reqwest::Client` across all HTTP-mail transports.
/// Connection-pooled, rustls, no PII headers. Carries an explicit
/// request + connect timeout so a slow or unresponsive provider cannot
/// hold a `MailTransport::send` await indefinitely.
pub(crate) fn shared_client() -> &'static Client {
    static CLIENT: OnceLock<Client> = OnceLock::new();
    CLIENT.get_or_init(|| {
        Client::builder()
            .user_agent(concat!("suprnova-mail/", env!("CARGO_PKG_VERSION")))
            .timeout(DEFAULT_REQUEST_TIMEOUT)
            .connect_timeout(DEFAULT_CONNECT_TIMEOUT)
            .build()
            .expect("reqwest client builder")
    })
}

pub(crate) fn err(provider: &'static str, status: u16, body: String) -> FrameworkError {
    FrameworkError::internal(format!("{provider} HTTP {status}: {body}"))
}
