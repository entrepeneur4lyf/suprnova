//! Mail subsystem.

pub mod address;
pub mod boot;
pub(crate) mod http_provider;
pub mod log;
pub mod mailable;
pub mod mailable_registry;
pub mod mailgun;
pub mod memory;
pub mod postmark;
pub mod resend;
pub mod send_job;
pub mod sendgrid;
pub mod ses;
pub mod smtp;
pub mod transport;

pub use address::{Address, Attachment};
pub use mailable::{register_mailable_factory, Mailable};
pub use send_job::SendMailJob;
pub use transport::{MailTransport, OutgoingMessage};

use crate::error::FrameworkError;
use std::sync::{Arc, RwLock};

static TRANSPORT: RwLock<Option<Arc<dyn MailTransport>>> = RwLock::new(None);

pub struct Mail;

impl Mail {
    pub fn set_transport(transport: Arc<dyn MailTransport>) {
        *TRANSPORT.write().expect("mail transport lock poisoned") = Some(transport);
    }
    pub fn clear_transport() {
        *TRANSPORT.write().expect("mail transport lock poisoned") = None;
    }

    pub fn to(addr: impl Into<Address>) -> MailBuilder {
        MailBuilder::default().to(addr)
    }

    pub(crate) fn current_transport() -> Result<Arc<dyn MailTransport>, FrameworkError> {
        TRANSPORT
            .read()
            .expect("mail transport lock poisoned")
            .clone()
            .ok_or_else(|| FrameworkError::internal(
                "no mail transport configured; call Mail::set_transport(...) or run Mail::bootstrap_from_env()"
            ))
    }
}

#[derive(Default, Debug)]
pub struct MailBuilder {
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    reply_to: Vec<Address>,
    from_override: Option<Address>,
}

impl MailBuilder {
    pub fn to(mut self, addr: impl Into<Address>) -> Self { self.to.push(addr.into()); self }
    pub fn cc(mut self, addr: impl Into<Address>) -> Self { self.cc.push(addr.into()); self }
    pub fn bcc(mut self, addr: impl Into<Address>) -> Self { self.bcc.push(addr.into()); self }
    pub fn reply_to(mut self, addr: impl Into<Address>) -> Self { self.reply_to.push(addr.into()); self }
    pub fn from(mut self, addr: impl Into<Address>) -> Self { self.from_override = Some(addr.into()); self }

    /// Render `mailable` and dispatch to the bound transport.
    pub async fn send<M: Mailable>(self, mailable: M) -> Result<(), FrameworkError> {
        let transport = Mail::current_transport()?;
        let from = self.from_override
            .or_else(|| mailable.from())
            .unwrap_or_else(|| Address::new("noreply@localhost"));

        let html = mailable.render_html()?;
        let text = mailable.render_text()?;

        if html.is_none() && text.is_none() {
            return Err(FrameworkError::internal(format!(
                "mail: {} has no text or html body — define text_template_source or html_template_source on the Mailable",
                M::mailable_name()
            )));
        }

        let msg = OutgoingMessage {
            from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            reply_to: self.reply_to,
            subject: mailable.subject(),
            html,
            text,
            attachments: mailable.attachments(),
        };

        transport.send(&msg).await
    }

    /// Build a [`SendMailJob`] and push it onto the queue. The mailable's
    /// concrete type must be registered via
    /// [`register_mailable_factory`](crate::mail::register_mailable_factory)
    /// before the worker dispatches the job.
    ///
    /// Fails fast (push-time, before any envelope is created) if the
    /// mailable defines neither `html_template_source` nor
    /// `text_template_source`. This mirrors `MailBuilder::send`'s
    /// empty-body guard so a misconfigured mailable cannot silently emit
    /// blank messages through the queue path. The same guard runs again
    /// inside `mailable_registry::render_outgoing` as defense in depth.
    pub async fn queue<M: Mailable>(self, mailable: M) -> Result<(), FrameworkError> {
        let job = self.build_send_job(mailable)?;
        crate::queue::Queue::push(job).await
    }

    /// Queue the mailable for a delayed dispatch. Same empty-body guard
    /// and registry requirements as [`MailBuilder::queue`].
    pub async fn later<M: Mailable>(
        self,
        delay: std::time::Duration,
        mailable: M,
    ) -> Result<(), FrameworkError> {
        let job = self.build_send_job(mailable)?;
        crate::queue::Queue::later(delay, job).await
    }

    fn build_send_job<M: Mailable>(
        self,
        mailable: M,
    ) -> Result<SendMailJob, FrameworkError> {
        // Match `MailBuilder::send`'s guard exactly: call the trait-level
        // `render_html`/`render_text` so a Mailable that overrides those
        // methods (e.g. produces a pre-rendered body without setting a
        // template source) is accepted here too. Checking only the raw
        // `*_template_source` getters would diverge from the sync `send`
        // path and reject valid overrides.
        let html = mailable.render_html()?;
        let text = mailable.render_text()?;
        if html.is_none() && text.is_none() {
            return Err(FrameworkError::internal(format!(
                "mail: {} has no text or html body — define text_template_source or html_template_source on the Mailable",
                M::mailable_name()
            )));
        }
        let payload = serde_json::to_value(&mailable)
            .map_err(|e| FrameworkError::internal(format!("Mail::queue encode: {e}")))?;
        Ok(SendMailJob {
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            reply_to: self.reply_to,
            from_override: self.from_override,
            mailable_name: M::mailable_name().to_string(),
            mailable_payload: payload,
        })
    }
}
