//! Mailable trait + registry.
//!
//! A `Mailable` is a serializable struct that knows its subject, template
//! source(s), optional sender override, attachments, and per-message
//! provider hints (tags, metadata, priority, custom headers, return path).
//! Mail::send and Mail::queue both consume Mailable; the queue path stores
//! the mailable JSON in the FROZEN envelope and reconstructs via the
//! registry on dispatch.
//!
//! # Template syntax
//!
//! Templates use [Tera](https://keats.github.io/tera/). The mailable's
//! serialized fields are the rendering context — every `pub` field on the
//! struct is reachable as `{{ field_name }}`.
//!
//! Autoescape is OFF because mail bodies are typically hand-authored HTML
//! where Tera's `<>&` escaping would over-escape. **If your literal body
//! contains `{{` for non-template reasons** (e.g., marketing copy quoting
//! Mustache syntax), escape it: `{% raw %}{{ literal }}{% endraw %}`.

use crate::error::FrameworkError;
use crate::mail::address::{Address, Attachment};
use async_trait::async_trait;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;

/// User-defined outgoing message. Implementations declare a stable
/// `mailable_name`, a subject + optional Tera-templated bodies, and
/// optional per-provider hints (tags, metadata, attachments, …). The
/// dispatcher renders the body, applies global defaults, and ships to
/// the bound [`MailTransport`](crate::mail::transport::MailTransport).
#[async_trait]
pub trait Mailable: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable name used in the queue envelope. Renaming breaks in-flight messages.
    fn mailable_name() -> &'static str
    where
        Self: Sized;

    /// Computed subject. Used as the rendered subject when
    /// [`subject_template_source`](Self::subject_template_source) returns
    /// `None`. Use `format!` for runtime substitution, or override
    /// `subject_template_source` to get Tera-style `{{ field }}`
    /// interpolation with the mailable's serialized fields as the context.
    fn subject(&self) -> String;

    /// Subject Tera template source. When `Some`, takes precedence over
    /// [`subject`](Self::subject) and is rendered through Tera with the
    /// mailable's serialized fields as the context — same semantics as
    /// [`html_template_source`](Self::html_template_source) and
    /// [`text_template_source`](Self::text_template_source).
    ///
    /// This lets `Mailable` impls write Tera-templated subjects without
    /// duplicating field-formatting logic, and brings the trait's three
    /// rendering paths (subject / html / text) onto one consistent
    /// surface. The defaulted `None` keeps existing impls that only
    /// override `subject()` working unchanged.
    fn subject_template_source(&self) -> Option<String> {
        None
    }

    /// HTML template source (Tera syntax). Return None to skip HTML.
    fn html_template_source(&self) -> Option<String> {
        None
    }

    /// Plain-text template source. Return None to skip plaintext.
    fn text_template_source(&self) -> Option<String> {
        None
    }

    /// Override the global default `from` for this mailable.
    fn from(&self) -> Option<Address> {
        None
    }

    /// Attachments to include with every dispatch. Default empty.
    fn attachments(&self) -> Vec<Attachment> {
        Vec::new()
    }

    /// Provider tags — matches Laravel `Mailable::tag()`. Postmark `Tag`,
    /// SES `Tags`, SendGrid `categories`, Mailgun `o:tag`, Resend `tags`.
    /// Default empty.
    fn tags(&self) -> Vec<String> {
        Vec::new()
    }

    /// Provider metadata — matches Laravel `Mailable::metadata()`.
    /// Postmark `Metadata`, SES `Tags` (k/v), SendGrid `custom_args`,
    /// Mailgun `v:` prefixed variables, Resend headers. Default empty.
    fn metadata(&self) -> BTreeMap<String, String> {
        BTreeMap::new()
    }

    /// Message priority (1 = highest, 5 = lowest). Matches Laravel
    /// `Mailable::priority()`. Default `None` (unset).
    fn priority(&self) -> Option<u8> {
        None
    }

    /// Custom MIME headers. Matches Laravel's envelope `Headers` /
    /// `withSymfonyMessage(fn ($m) => $m->getHeaders()->add(...))`.
    /// Default empty.
    fn headers(&self) -> Vec<(String, String)> {
        Vec::new()
    }

    /// Return-Path / Bounce-To address. Matches Laravel
    /// `Mailable::alwaysReturnPath` (per-mailable variant). Default `None`.
    fn return_path(&self) -> Option<Address> {
        None
    }

    /// Render the subject. When `subject_template_source` returns `Some`,
    /// Tera-renders that template with `self` as the context; otherwise
    /// returns `subject()` unchanged. The dispatch path
    /// (`MailBuilder::send`, the queue worker, the notification mail
    /// channel) calls this — so a Tera-templated subject in any of the
    /// supported surfaces renders correctly without each call site
    /// reaching into the template source itself.
    fn render_subject(&self) -> Result<String, FrameworkError> {
        let Some(src) = self.subject_template_source() else {
            return Ok(self.subject());
        };
        render_with_self(self, &src, "subject")
    }

    /// Render the HTML body. Default impl uses Tera with the mailable's
    /// serialized fields as the context. Override if you need custom rendering
    /// (e.g., Markdown → HTML, or a pre-rendered string from elsewhere).
    fn render_html(&self) -> Result<Option<String>, FrameworkError> {
        let Some(src) = self.html_template_source() else {
            return Ok(None);
        };
        render_with_self(self, &src, "html").map(Some)
    }

    /// Render the plaintext body. Same defaulting behavior as `render_html`.
    fn render_text(&self) -> Result<Option<String>, FrameworkError> {
        let Some(src) = self.text_template_source() else {
            return Ok(None);
        };
        render_with_self(self, &src, "text").map(Some)
    }
}

fn render_with_self<M: Mailable>(
    mailable: &M,
    source: &str,
    label: &'static str,
) -> Result<String, FrameworkError> {
    let value = serde_json::to_value(mailable)
        .map_err(|e| FrameworkError::internal(format!("mail: encode mailable ({label}): {e}")))?;
    let ctx = tera::Context::from_value(value)
        .map_err(|e| FrameworkError::internal(format!("mail: Tera context ({label}): {e}")))?;
    tera::Tera::one_off(source, &ctx, false)
        .map_err(|e| FrameworkError::internal(format!("mail: Tera template ({label}): {e}")))
}

/// Register a [`Mailable`] type for queue dispatch. Call once at boot for
/// every concrete mailable that is reachable via `Mail::queue` or
/// `Mail::later`. The worker rebuilds the mailable through this registry
/// using `mailable_name` as the lookup key; an unregistered mailable
/// surfaces as `unknown mailable: {name}` and either retries or
/// dead-letters per the envelope's backoff policy.
///
/// Re-registering the same name silently replaces the existing factory
/// (last-write-wins) — matches the queue worker registry and the
/// notification dispatcher's channel registration.
pub fn register_mailable_factory<M: Mailable>() -> Result<(), FrameworkError> {
    crate::mail::mailable_registry::register::<M>()
}
