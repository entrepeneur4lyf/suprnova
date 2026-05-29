//! Mailable registry — maps `mailable_name` to a deserializer for queue dispatch.
//!
//! [`AnyMailable`] is the object-safe shadow of [`Mailable`]: it forwards
//! `render_html` / `render_text` / `subject` / `from` / `attachments` /
//! `tags` / `metadata` / `priority` / `headers` / `return_path` so the
//! queued path (which only has a boxed `Box<dyn AnyMailable>` after
//! factory deserialization) still gets the mailable's full Tera context
//! via the trait's defaulted render methods AND the per-provider hints.
//! The factory closure captures the concrete `M`, so the trait-method
//! dispatch through the box preserves access to `serde_json::to_value(self)`
//! for the rendering context.
//!
//! # v1 simplification: function pointers, not boxed `Fn`
//!
//! [`Factory`] is a `fn(...)` function pointer, not `Box<dyn Fn>`. This
//! covers every callsite today (the registered factories never capture
//! state). If a future caller needs to capture (e.g. a per-app context
//! handle), bump this to `Arc<dyn Fn>` — that change is local to this
//! module and the call sites in `send_job::SendMailJob::handle`.

use crate::error::FrameworkError;
use crate::lock;
use crate::mail::address::{Address, Attachment};
use crate::mail::mailable::Mailable;
use crate::mail::transport::OutgoingMessage;
use std::collections::{BTreeMap, HashMap};
use std::sync::RwLock;

/// Factory type for the mailable registry. v1 uses `fn(...)` rather than
/// `Arc<dyn Fn>` because registered factories are stateless and a function
/// pointer keeps clone/copy ergonomics trivial. Bump to `Arc<dyn Fn>` if a
/// future caller needs to capture state.
pub(crate) type Factory = fn(serde_json::Value) -> Result<Box<dyn AnyMailable>, FrameworkError>;

/// Object-safe view of [`Mailable`]. The blanket impl below forwards every
/// method to the concrete `M`, including the Tera-backed `render_html` /
/// `render_text` / `render_subject` defaults — so the queued path never
/// sees raw template source, only rendered output.
pub trait AnyMailable: Send + Sync {
    fn render_subject(&self) -> Result<String, FrameworkError>;
    fn render_html(&self) -> Result<Option<String>, FrameworkError>;
    fn render_text(&self) -> Result<Option<String>, FrameworkError>;
    fn from(&self) -> Option<Address>;
    fn attachments(&self) -> Vec<Attachment>;
    fn tags(&self) -> Vec<String>;
    fn metadata(&self) -> BTreeMap<String, String>;
    fn priority(&self) -> Option<u8>;
    fn headers(&self) -> Vec<(String, String)>;
    fn return_path(&self) -> Option<Address>;
}

impl<M: Mailable> AnyMailable for M {
    fn render_subject(&self) -> Result<String, FrameworkError> {
        <M as Mailable>::render_subject(self)
    }
    fn render_html(&self) -> Result<Option<String>, FrameworkError> {
        <M as Mailable>::render_html(self)
    }
    fn render_text(&self) -> Result<Option<String>, FrameworkError> {
        <M as Mailable>::render_text(self)
    }
    fn from(&self) -> Option<Address> {
        <M as Mailable>::from(self)
    }
    fn attachments(&self) -> Vec<Attachment> {
        <M as Mailable>::attachments(self)
    }
    fn tags(&self) -> Vec<String> {
        <M as Mailable>::tags(self)
    }
    fn metadata(&self) -> BTreeMap<String, String> {
        <M as Mailable>::metadata(self)
    }
    fn priority(&self) -> Option<u8> {
        <M as Mailable>::priority(self)
    }
    fn headers(&self) -> Vec<(String, String)> {
        <M as Mailable>::headers(self)
    }
    fn return_path(&self) -> Option<Address> {
        <M as Mailable>::return_path(self)
    }
}

static REGISTRY: RwLock<Option<HashMap<String, Factory>>> = RwLock::new(None);

/// Register `M` under its `mailable_name`. Last-write-wins: re-registering
/// the same name silently replaces the factory (matches the queue worker
/// registry and the dispatcher channel registry).
pub fn register<M: Mailable>() -> Result<(), FrameworkError> {
    let factory: Factory = |payload| {
        let m: M = serde_json::from_value(payload).map_err(|e| {
            FrameworkError::internal(format!("decode mailable {}: {e}", M::mailable_name()))
        })?;
        Ok(Box::new(m))
    };
    let mut g = lock::write(&REGISTRY)?;
    g.get_or_insert_with(HashMap::new)
        .insert(M::mailable_name().to_string(), factory);
    Ok(())
}

/// Decode a payload using the factory registered under `name`. Returns
/// `Err` if `name` is unknown — the worker surfaces that error and either
/// retries or dead-letters per the envelope's backoff policy.
pub fn build(
    name: &str,
    payload: serde_json::Value,
) -> Result<Box<dyn AnyMailable>, FrameworkError> {
    let g = lock::read(&REGISTRY)?;
    let map = g
        .as_ref()
        .ok_or_else(|| FrameworkError::internal(format!("unknown mailable: {name}")))?;
    let factory = map
        .get(name)
        .ok_or_else(|| FrameworkError::internal(format!("unknown mailable: {name}")))?;
    factory(payload)
}

/// Build an [`OutgoingMessage`] from a registered mailable. The mailable's
/// `render_html` / `render_text` (defaulted on the trait) run Tera with the
/// mailable's serialized fields as the context — identical to the sync
/// `Mail::send` path.
///
/// `mailable_name` is passed in so the empty-body guard can report the
/// concrete mailable type by name (the object-safe `AnyMailable` trait
/// cannot expose `M::mailable_name()` because that method requires
/// `Self: Sized`).
#[allow(clippy::too_many_arguments)]
pub fn render_outgoing(
    any: &dyn AnyMailable,
    mailable_name: &str,
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    reply_to: Vec<Address>,
    from_override: Option<Address>,
    extra_tags: Vec<String>,
    extra_metadata: BTreeMap<String, String>,
    extra_priority: Option<u8>,
    extra_headers: Vec<(String, String)>,
    return_path_override: Option<Address>,
) -> Result<OutgoingMessage, FrameworkError> {
    let from = from_override
        .or_else(|| any.from())
        .unwrap_or_else(|| Address::new("noreply@localhost"));
    let html = any.render_html()?;
    let text = any.render_text()?;
    if html.is_none() && text.is_none() {
        return Err(FrameworkError::internal(format!(
            "mail: {mailable_name} has no text or html body — define text_template_source or html_template_source on the Mailable"
        )));
    }

    // Merge mailable-level + builder-level hints. Builder wins on key
    // collisions in `metadata`; tags / headers union and de-dupe.
    let mut tags = any.tags();
    for t in extra_tags {
        if !tags.contains(&t) {
            tags.push(t);
        }
    }
    let mut metadata = any.metadata();
    for (k, v) in extra_metadata {
        metadata.insert(k, v);
    }
    let mut headers = any.headers();
    for (k, v) in extra_headers {
        if !headers.iter().any(|(hk, hv)| hk == &k && hv == &v) {
            headers.push((k, v));
        }
    }
    let priority = extra_priority.or_else(|| any.priority());
    let return_path = return_path_override.or_else(|| any.return_path());

    Ok(OutgoingMessage {
        from,
        to,
        cc,
        bcc,
        reply_to,
        subject: any.render_subject()?,
        html,
        text,
        attachments: any.attachments(),
        tags,
        metadata,
        priority,
        headers,
        return_path,
    })
}
