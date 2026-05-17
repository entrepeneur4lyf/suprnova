//! Mailable trait + registry.
//!
//! A `Mailable` is a serializable struct that knows its subject, template
//! source(s), optional sender override, and attachments. Mail::send and
//! Mail::queue both consume Mailable; the queue path stores the mailable
//! JSON in the FROZEN envelope and reconstructs via the registry on dispatch.
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
use serde::{de::DeserializeOwned, Serialize};

#[async_trait]
pub trait Mailable: Serialize + DeserializeOwned + Send + Sync + 'static {
    /// Stable name used in the queue envelope. Renaming breaks in-flight messages.
    fn mailable_name() -> &'static str where Self: Sized;

    fn subject(&self) -> String;

    /// HTML template source (Tera syntax). Return None to skip HTML.
    fn html_template_source(&self) -> Option<String> { None }

    /// Plain-text template source. Return None to skip plaintext.
    fn text_template_source(&self) -> Option<String> { None }

    /// Override the global default `from` for this mailable.
    fn from(&self) -> Option<Address> { None }

    fn attachments(&self) -> Vec<Attachment> { Vec::new() }

    /// Render the HTML body. Default impl uses Tera with the mailable's
    /// serialized fields as the context. Override if you need custom rendering
    /// (e.g., Markdown → HTML, or a pre-rendered string from elsewhere).
    fn render_html(&self) -> Result<Option<String>, FrameworkError> {
        let Some(src) = self.html_template_source() else { return Ok(None); };
        render_with_self(self, &src, "html").map(Some)
    }

    /// Render the plaintext body. Same defaulting behavior as `render_html`.
    fn render_text(&self) -> Result<Option<String>, FrameworkError> {
        let Some(src) = self.text_template_source() else { return Ok(None); };
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

// Registry — fleshed out in Task 19 when queue integration lands.
#[allow(dead_code)]
pub(crate) fn register_mailable<M: Mailable>() {
    let _ = M::mailable_name();
}
