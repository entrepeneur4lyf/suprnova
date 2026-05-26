use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Hello {
    name: String,
}

#[async_trait]
impl Mailable for Hello {
    fn mailable_name() -> &'static str {
        "Hello"
    }
    fn subject(&self) -> String {
        format!("Hi {}", self.name)
    }
    fn text_template_source(&self) -> Option<String> {
        Some(format!("Hello {}!", self.name))
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn in_memory_transport_captures_outgoing_messages() {
    let transport = Arc::new(InMemoryMailTransport::new());
    Mail::set_transport(transport.clone());

    Mail::to("alice@example.org")
        .send(Hello {
            name: "Alice".into(),
        })
        .await
        .unwrap();

    let captured = transport.captured();
    assert_eq!(captured.len(), 1);
    let msg = &captured[0];
    assert_eq!(msg.subject, "Hi Alice");
    assert_eq!(msg.text.as_deref(), Some("Hello Alice!"));
    assert_eq!(msg.to[0].email, "alice@example.org");
    assert_eq!(msg.from.email, "noreply@suprnova.dev");
}

#[tokio::test]
#[serial]
async fn mail_send_errors_when_no_transport_bound() {
    Mail::clear_transport();
    let err = Mail::to("alice@example.org")
        .send(Hello {
            name: "Alice".into(),
        })
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("mail transport"), "got: {s}");
}
