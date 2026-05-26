//! WelcomeEmail mailable + a small helper that queues it.
//!
//! Dogfood for Phase 5B Task 20. The helper exists so the route handler
//! and the integration test can share the same `Mail::queue` call without
//! re-stating the recipient plumbing.

use serde::{Deserialize, Serialize};
use suprnova::FrameworkError;
use suprnova::async_trait;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct WelcomeEmail {
    pub name: String,
}

#[async_trait]
impl Mailable for WelcomeEmail {
    fn mailable_name() -> &'static str {
        "WelcomeEmail"
    }

    fn subject(&self) -> String {
        format!("Welcome to Suprnova, {}", self.name)
    }

    fn html_template_source(&self) -> Option<String> {
        // Tera context exposes every public field on the struct, so `{{ name }}`
        // resolves to `self.name`. Keep the template literal — the renderer
        // substitutes it rather than the format-string indirection used by an
        // earlier draft.
        Some("<h1>Hi {{ name }}!</h1><p>Glad you're here.</p>".to_string())
    }

    fn text_template_source(&self) -> Option<String> {
        Some("Hi {{ name }}!\nGlad you're here.".to_string())
    }

    fn from(&self) -> Option<Address> {
        Some("hello@suprnova.dev".into())
    }
}

/// Queue a WelcomeEmail for `(email, name)` via the bound mail transport.
///
/// Shared between the `POST /api/welcome` route handler and the
/// `mail_welcome_dogfood` integration test so the queue-envelope shape is
/// exercised the same way in both call sites.
pub async fn queue_welcome(email: &str, name: &str) -> Result<(), FrameworkError> {
    Mail::to(email)
        .queue(WelcomeEmail { name: name.into() })
        .await
}
