use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::log::LogMailTransport;
use suprnova::mail::{Address, Mail, Mailable};
use tracing_test::traced_test;

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Ping {
    msg: String,
}

#[async_trait]
impl Mailable for Ping {
    fn mailable_name() -> &'static str { "Ping" }
    fn subject(&self) -> String { "ping".into() }
    fn text_template_source(&self) -> Option<String> { Some("pong".into()) }
    fn from(&self) -> Option<Address> { Some("noreply@suprnova.dev".into()) }
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct EmptyBody;

#[async_trait]
impl Mailable for EmptyBody {
    fn mailable_name() -> &'static str { "EmptyBody" }
    fn subject(&self) -> String { "nope".into() }
}

#[tokio::test]
#[serial]
#[traced_test]
async fn log_transport_emits_event_with_message_fields() {
    Mail::set_transport(Arc::new(LogMailTransport::new()));
    Mail::to("alice@example.org")
        .send(Ping { msg: "hello".into() })
        .await
        .unwrap();
    assert!(logs_contain("mail (log driver): would send"));
    assert!(logs_contain("alice@example.org"));
    assert!(logs_contain("noreply@suprnova.dev"));
    assert!(logs_contain("ping"));
}

#[tokio::test]
#[serial]
async fn mailbuilder_rejects_mailable_without_any_body() {
    Mail::set_transport(Arc::new(LogMailTransport::new()));
    let err = Mail::to("alice@example.org")
        .send(EmptyBody)
        .await
        .unwrap_err();
    let msg = format!("{err}");
    assert!(msg.contains("EmptyBody"), "error mentions the Mailable name: {msg}");
    assert!(msg.contains("text_template_source") || msg.contains("html_template_source"),
        "error suggests which methods to implement: {msg}");
}
