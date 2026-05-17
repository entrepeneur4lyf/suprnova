//! Live SMTP integration test. Requires a Mailpit (or equivalent) SMTP
//! server reachable at `MAIL_SMTP_HOST:MAIL_SMTP_PORT` (defaults to
//! 127.0.0.1:1025).
//!
//! Run with `cargo test -p suprnova --test mail_smtp -- --ignored`.

use serde::{Deserialize, Serialize};
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::smtp::SmtpMailTransport;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct LiveHello {
    msg: String,
}

#[async_trait]
impl Mailable for LiveHello {
    fn mailable_name() -> &'static str { "LiveHello" }
    fn subject(&self) -> String { "live-test".into() }
    fn text_template_source(&self) -> Option<String> { Some("hello".into()) }
    fn from(&self) -> Option<Address> { Some("noreply@suprnova.dev".into()) }
}

#[ignore = "requires a real SMTP server (Mailpit at 127.0.0.1:1025 by default)"]
#[tokio::test]
async fn smtp_transport_sends_through_live_server() {
    let host = std::env::var("MAIL_SMTP_HOST").unwrap_or_else(|_| "127.0.0.1".into());
    let port: u16 = std::env::var("MAIL_SMTP_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1025);
    let transport = SmtpMailTransport::unencrypted(&host, port).unwrap();
    Mail::set_transport(Arc::new(transport));
    Mail::to("recipient@example.org")
        .send(LiveHello { msg: "hello".into() })
        .await
        .unwrap();
}
