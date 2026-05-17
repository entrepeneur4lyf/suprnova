//! Mail notification channel — delivers via the bound mail transport.
//!
//! [`MailChannel`] is stateless. Each notification that wants to be
//! delivered via mail opts in by implementing [`NotificationMailable`]
//! and registering its renderer once at boot via
//! [`register_mail_renderer`]. At dispatch time, the channel looks up
//! the renderer by `Notification::notification_name()`, deserializes
//! the JSON payload back into the concrete `N`, and invokes
//! `N::to_mail(&self)` to produce a [`MailRendering`]. The channel
//! then assembles an `OutgoingMessage` addressed to the route
//! returned by the `Notifiable` and dispatches it through
//! `Mail::current_transport`.
//!
//! Empty-body guard: if the renderer returns a rendering with neither
//! `html` nor `text`, delivery fails fast with a clear error. This
//! mirrors `MailBuilder::send`'s upstream check on the `Mailable` path
//! — we refuse to silently dispatch blank emails through any code path.
//!
//! Why a per-Notification trait rather than a single factory closure
//! at construction time: the closure approach centralized rendering
//! logic in one match-on-name and lost type safety on the JSON
//! payload. The trait gives each notification ownership of its own
//! mail representation, matches Laravel's `toMail()` idiom, and hands
//! the renderer a serde-deserialized concrete type instead of raw
//! JSON.

use crate::error::FrameworkError;
use crate::mail::transport::OutgoingMessage;
use crate::mail::{Address, Attachment, Mail};
use crate::notifications::{Channel, DynNotification, Notification};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::RwLock;

/// What a per-notification renderer must produce — enough to assemble
/// an outgoing message. `subject` is required; at least one of `html` /
/// `text` must be `Some` or delivery will fail. `from` is optional and
/// falls back to `noreply@localhost` to match `MailBuilder::send`.
///
/// `cc`, `bcc`, `reply_to`, and `attachments` are optional and default
/// to empty. Use `..Default::default()` in the struct literal to skip
/// any field you don't need:
///
/// ```ignore
/// MailRendering {
///     subject: "Order shipped".into(),
///     text: Some("Tracking: 1Z999".into()),
///     ..Default::default()
/// }
/// ```
#[derive(Default)]
pub struct MailRendering {
    pub subject: String,
    pub html: Option<String>,
    pub text: Option<String>,
    pub from: Option<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub reply_to: Vec<Address>,
    pub attachments: Vec<Attachment>,
}

/// Opt-in trait for Notifications that want to be deliverable via the
/// mail channel.
///
/// The Notification owns its mail representation — `to_mail` produces
/// the rendered subject/body content. No `Notifiable` argument: the
/// queued path loses the original `Notifiable`, so per-recipient
/// variation must ride through the Notification's `data()` (the
/// payload is serialized at queue time and reconstructed before
/// `to_mail` runs).
///
/// Bootstrap registers each implementor once via
/// [`register_mail_renderer::<N>()`]. The [`MailChannel`] then looks
/// up the renderer by `N::notification_name()` at dispatch time.
pub trait NotificationMailable: Notification {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError>;
}

/// Renderer function pointer. v1 uses `fn(...)` rather than
/// `Arc<dyn Fn>` because registered renderers are stateless — every
/// renderer is the monomorphized closure produced by
/// [`register_mail_renderer`], which only closes over the type
/// parameter `N`. Bump to `Arc<dyn Fn>` if a future caller needs to
/// capture state.
type MailRendererFn = fn(serde_json::Value) -> Result<MailRendering, FrameworkError>;

static MAIL_RENDERERS: RwLock<Option<HashMap<&'static str, MailRendererFn>>> = RwLock::new(None);

/// Register a Notification's mail renderer. The [`MailChannel`] uses
/// the notification name (from `Notification::notification_name()`)
/// as the registry key.
///
/// Re-registering the same name silently replaces the existing
/// renderer (last-write-wins) — matches the notification factory
/// registry and the dispatcher's channel registration.
pub fn register_mail_renderer<N: NotificationMailable>() {
    let renderer: MailRendererFn = |payload| {
        let n: N = serde_json::from_value(payload).map_err(|e| {
            FrameworkError::internal(format!("decode {}: {e}", N::notification_name()))
        })?;
        n.to_mail()
    };
    let mut g = MAIL_RENDERERS
        .write()
        .expect("mail renderer registry poisoned");
    g.get_or_insert_with(HashMap::new)
        .insert(N::notification_name(), renderer);
}

fn renderer_for(name: &str) -> Result<MailRendererFn, FrameworkError> {
    let missing = || {
        FrameworkError::internal(format!(
            "no mail renderer for notification {name} — register via suprnova::register_mail_renderer::<N>()"
        ))
    };
    let g = MAIL_RENDERERS
        .read()
        .expect("mail renderer registry poisoned");
    // Treat "registry never initialized" identically to "this notification
    // not registered" — the operator-facing fix is the same.
    let map = g.as_ref().ok_or_else(missing)?;
    map.get(name).copied().ok_or_else(missing)
}

/// Notification channel that delivers via the bound mail transport.
///
/// Stateless — construction takes no arguments. At dispatch time the
/// channel looks up the per-notification renderer in the global
/// registry populated by [`register_mail_renderer`].
///
/// `cc`, `bcc`, `reply_to`, and `attachments` ride through
/// [`MailRendering`] — populate any of them in `to_mail` and the
/// channel threads them into the outgoing message verbatim.
pub struct MailChannel;

impl MailChannel {
    pub fn new() -> Self {
        Self
    }
}

impl Default for MailChannel {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Channel for MailChannel {
    fn name(&self) -> &'static str {
        "mail"
    }

    async fn deliver(
        &self,
        route: &str,
        notification: &dyn DynNotification,
    ) -> Result<(), FrameworkError> {
        let renderer = renderer_for(notification.name())?;
        let rendering = renderer(notification.data())?;

        // Empty-body guard — mirror MailBuilder::send's upstream check
        // so notification dispatch can never silently send a blank
        // email. Runs BEFORE current_transport() so a missing
        // transport doesn't mask a misconfigured renderer.
        if rendering.html.is_none() && rendering.text.is_none() {
            return Err(FrameworkError::internal(format!(
                "MailChannel: renderer for {} returned no html or text body",
                notification.name()
            )));
        }

        let from = rendering
            .from
            .unwrap_or_else(|| Address::new("noreply@localhost"));
        let msg = OutgoingMessage {
            from,
            to: vec![route.into()],
            cc: rendering.cc,
            bcc: rendering.bcc,
            reply_to: rendering.reply_to,
            subject: rendering.subject,
            html: rendering.html,
            text: rendering.text,
            attachments: rendering.attachments,
        };

        let transport = Mail::current_transport()?;
        transport.send(&msg).await
    }
}
