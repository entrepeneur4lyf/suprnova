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
use crate::mail::memory::InMemoryMailTransport;
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

    /// Install an in-memory capture transport for the duration of the
    /// returned guard. Mirrors `Bus::fake()` / `Queue::fake()` /
    /// `Cache::fake()` — tests call `Mail::fake()`, dispatch mail
    /// normally, then assert against the captured messages.
    ///
    /// The previously-bound transport (if any) is saved and restored
    /// when the [`MailFake`] guard drops. Tests can freely intermix
    /// `Mail::fake()` with other transport-mutating code without
    /// leaking state.
    ///
    /// ```ignore
    /// let fake = Mail::fake();
    /// Mail::to("alice@example.org").send(welcome).await?;
    /// fake.assert_sent(|m| m.subject.contains("Welcome"));
    /// fake.assert_sent_count(1);
    /// // fake drops here — previous transport (or absence) is restored.
    /// ```
    pub fn fake() -> MailFake {
        let transport = Arc::new(InMemoryMailTransport::new());
        let previous = TRANSPORT
            .write()
            .expect("mail transport lock poisoned")
            .replace(transport.clone() as Arc<dyn MailTransport>);
        MailFake { transport, previous }
    }

    pub(crate) fn current_transport() -> Result<Arc<dyn MailTransport>, FrameworkError> {
        TRANSPORT
            .read()
            .expect("mail transport lock poisoned")
            .clone()
            .ok_or_else(|| FrameworkError::internal(
                "no mail transport configured; call Mail::set_transport(...) or run suprnova::mail::boot::bootstrap_from_env()"
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

/// RAII guard returned by [`Mail::fake`]. Captures every dispatched
/// outgoing message in memory and restores the previously-bound
/// transport when dropped.
///
/// `MailFake` is `Send + Sync` — tests can share it across awaits or
/// threads if they need to, though the typical pattern is a single
/// `let fake = Mail::fake();` at the top of the test.
pub struct MailFake {
    transport: Arc<InMemoryMailTransport>,
    previous: Option<Arc<dyn MailTransport>>,
}

impl MailFake {
    /// All messages captured since the fake was installed.
    pub fn captured(&self) -> Vec<OutgoingMessage> {
        self.transport.captured()
    }

    /// Number of messages captured. Convenience over `captured().len()`.
    pub fn count(&self) -> usize {
        self.transport.captured().len()
    }

    /// Assert at least one captured message matches `predicate`.
    /// Panics with the full captured set if no match is found.
    pub fn assert_sent<F>(&self, predicate: F)
    where
        F: Fn(&OutgoingMessage) -> bool,
    {
        let captured = self.transport.captured();
        if !captured.iter().any(&predicate) {
            panic!(
                "Mail::fake assertion failed: expected at least one message matching predicate; \
                 captured {} message(s): {:#?}",
                captured.len(),
                captured
            );
        }
    }

    /// Assert NO captured message matches `predicate`.
    pub fn assert_not_sent<F>(&self, predicate: F)
    where
        F: Fn(&OutgoingMessage) -> bool,
    {
        let captured = self.transport.captured();
        if let Some(matching) = captured.iter().find(|m| predicate(m)) {
            panic!(
                "Mail::fake assertion failed: expected NO message matching predicate, \
                 found at least one: {matching:#?}"
            );
        }
    }

    /// Assert the captured count equals `expected`.
    pub fn assert_sent_count(&self, expected: usize) {
        let actual = self.transport.captured().len();
        assert_eq!(
            actual, expected,
            "Mail::fake: expected {expected} message(s), captured {actual}"
        );
    }
}

impl Drop for MailFake {
    fn drop(&mut self) {
        // Restore the previous transport. If a poisoned lock would
        // panic during drop we accept that — losing transport state
        // during teardown is preferable to silently leaving the fake
        // bound (which would corrupt every subsequent test).
        *TRANSPORT.write().expect("mail transport lock poisoned") = self.previous.take();
    }
}
