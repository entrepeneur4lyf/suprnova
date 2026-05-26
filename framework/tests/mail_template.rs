use serde::{Deserialize, Serialize};
use serial_test::serial;
use std::sync::Arc;
use suprnova::async_trait;
use suprnova::mail::memory::InMemoryMailTransport;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
struct WelcomeMail {
    name: String,
    link: String,
}

#[async_trait]
impl Mailable for WelcomeMail {
    fn mailable_name() -> &'static str {
        "WelcomeMail"
    }
    fn subject(&self) -> String {
        format!("Welcome, {}", self.name)
    }
    fn html_template_source(&self) -> Option<String> {
        Some("<h1>Hi {{ name }}!</h1><p><a href=\"{{ link }}\">verify</a></p>".into())
    }
    fn text_template_source(&self) -> Option<String> {
        Some("Hi {{ name }}!\nVerify: {{ link }}".into())
    }
    fn from(&self) -> Option<Address> {
        Some("noreply@suprnova.dev".into())
    }
}

#[tokio::test]
#[serial]
async fn mailable_renders_html_and_text_with_tera() {
    let transport = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(transport.clone());

    Mail::to("alice@example.org")
        .send(WelcomeMail {
            name: "Alice".into(),
            link: "https://example.org/v?t=abc".into(),
        })
        .await
        .unwrap();

    let msgs = transport.captured();
    assert_eq!(msgs.len(), 1);
    let m = &msgs[0];
    assert_eq!(m.subject, "Welcome, Alice");
    assert_eq!(
        m.html.as_deref(),
        Some("<h1>Hi Alice!</h1><p><a href=\"https://example.org/v?t=abc\">verify</a></p>")
    );
    assert_eq!(
        m.text.as_deref(),
        Some("Hi Alice!\nVerify: https://example.org/v?t=abc")
    );
}

#[tokio::test]
#[serial]
async fn mailable_render_error_is_framework_error() {
    #[derive(Serialize, Deserialize, Debug, Clone)]
    struct BadTemplate;

    #[async_trait]
    impl Mailable for BadTemplate {
        fn mailable_name() -> &'static str {
            "BadTemplate"
        }
        fn subject(&self) -> String {
            "x".into()
        }
        fn text_template_source(&self) -> Option<String> {
            // Unclosed {% tag %} → Tera parse error.
            Some("Hi {% if".into())
        }
        fn from(&self) -> Option<Address> {
            Some("a@b.c".into())
        }
    }

    let transport = Arc::new(InMemoryMailTransport::new());
    let _ = Mail::set_transport(transport.clone());

    let err = Mail::to("alice@example.org")
        .send(BadTemplate)
        .await
        .unwrap_err();
    let s = format!("{err}");
    assert!(s.contains("template") || s.contains("Tera"), "got: {s}");
}
