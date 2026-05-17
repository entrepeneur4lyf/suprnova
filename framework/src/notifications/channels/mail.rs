//! Mail notification channel — delivers via the bound mail transport.
//!
//! [`MailChannel`] is intentionally decoupled from any specific `Mailable`
//! type. The consumer supplies a factory closure that maps a notification's
//! name and JSON payload to a [`MailRendering`] (subject plus optional
//! html/text plus optional `from`). The channel then assembles an
//! `OutgoingMessage` addressed to the route returned by the `Notifiable`
//! and dispatches it through `Mail::current_transport`.
//!
//! Empty-body guard: if the factory returns a rendering with neither `html`
//! nor `text`, delivery fails fast with a clear error. This mirrors
//! `MailBuilder::send`'s upstream check on the `Mailable` path — we refuse
//! to silently dispatch blank emails through any code path.

use crate::error::FrameworkError;
use crate::mail::transport::OutgoingMessage;
use crate::mail::{Address, Mail};
use crate::notifications::{Channel, DynNotification};
use async_trait::async_trait;

/// What the per-notification factory must produce — enough to assemble
/// an outgoing message. `subject` is required; at least one of `html` /
/// `text` must be `Some` or delivery will fail. `from` is optional and
/// falls back to `noreply@localhost` to match `MailBuilder::send`.
pub struct MailRendering {
    pub subject: String,
    pub html: Option<String>,
    pub text: Option<String>,
    pub from: Option<Address>,
}

/// Signature of the factory closure that translates a notification's
/// `(name, data)` pair into a [`MailRendering`]. Returning `Err` aborts
/// delivery and surfaces verbatim through `NotificationDispatcher::notify`.
pub type MailFactory =
    dyn Fn(&str, serde_json::Value) -> Result<MailRendering, FrameworkError> + Send + Sync + 'static;

/// Notification channel that delivers via the bound mail transport.
///
/// Construction takes a factory closure receiving `(notification_name,
/// notification_data)` and returning a `MailRendering`. The factory is
/// the seam where consumers translate a notification type into a
/// concrete email body — typically by matching on the notification name
/// and rendering a Tera template per type.
pub struct MailChannel {
    factory: Box<MailFactory>,
}

impl MailChannel {
    pub fn new<F>(factory: F) -> Self
    where
        F: Fn(&str, serde_json::Value) -> Result<MailRendering, FrameworkError>
            + Send
            + Sync
            + 'static,
    {
        Self {
            factory: Box::new(factory),
        }
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
        let rendering = (self.factory)(notification.name(), notification.data())?;

        // Empty-body guard — mirror MailBuilder::send's upstream check so
        // notification dispatch can never silently send a blank email.
        // Runs BEFORE current_transport() so a missing transport doesn't
        // mask a misconfigured factory.
        if rendering.html.is_none() && rendering.text.is_none() {
            return Err(FrameworkError::internal(format!(
                "MailChannel: factory for {} returned no html or text body",
                notification.name()
            )));
        }

        let from = rendering
            .from
            .unwrap_or_else(|| Address::new("noreply@localhost"));
        let msg = OutgoingMessage {
            from,
            to: vec![route.into()],
            cc: Vec::new(),
            bcc: Vec::new(),
            reply_to: Vec::new(),
            subject: rendering.subject,
            html: rendering.html,
            text: rendering.text,
            attachments: Vec::new(),
        };

        let transport = Mail::current_transport()?;
        transport.send(&msg).await
    }
}
