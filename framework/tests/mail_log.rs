use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::log::LogMailTransport;
use suprnova::mail::{Address, Mail, Mailable};

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

#[tokio::test]
#[serial]
async fn log_transport_returns_ok_and_does_not_panic() {
    Mail::set_transport(Arc::new(LogMailTransport::new()));
    Mail::to("alice@example.org")
        .send(Ping { msg: "hello".into() })
        .await
        .unwrap();
}
