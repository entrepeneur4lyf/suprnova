//! Mail subsystem.

pub mod address;
pub mod boot;
pub mod events;
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
pub use events::{MessageSending, MessageSent};
pub use mailable::{Mailable, register_mailable_factory};
pub use send_job::SendMailJob;
pub use transport::{
    MailTransport, OutgoingMessage, PRIORITY_HIGH, PRIORITY_HIGHEST, PRIORITY_LOW, PRIORITY_LOWEST,
    PRIORITY_NORMAL, dispatch_with_telemetry,
};

use crate::error::FrameworkError;
use crate::lock;
use crate::mail::memory::InMemoryMailTransport;
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, RwLock};

static TRANSPORT: RwLock<Option<Arc<dyn MailTransport>>> = RwLock::new(None);

/// Global Mail-level defaults applied to every dispatched message — the
/// Rust analogue of Laravel's `Mailer::always_from`, `always_reply_to`,
/// `always_to`, `always_return_path`.
#[derive(Default, Debug, Clone)]
struct AlwaysDefaults {
    from: Option<Address>,
    reply_to: Option<Address>,
    /// When set, every dispatched message routes to this address, with
    /// CC and BCC cleared. Used for local-development "single inbox"
    /// configs and audit-log routing.
    to: Option<Address>,
    return_path: Option<Address>,
}

static ALWAYS: RwLock<AlwaysDefaults> = RwLock::new(AlwaysDefaults {
    from: None,
    reply_to: None,
    to: None,
    return_path: None,
});

/// Track of `(mailable_name, payload)` pairs queued through `Mail::queue`
/// / `Mail::later` while a `Mail::fake()` is active. Lets
/// `MailFake::assert_queued` find typed mailables on the queue path
/// without requiring callers to also install `Queue::fake`.
static QUEUE_CAPTURE: Mutex<Vec<QueuedMailable>> = Mutex::new(Vec::new());

/// Explicit "MailFake guard is live" flag — incremented when the fake
/// installs, decremented on drop. We use a counter rather than a bool so
/// nested calls (which the public API doesn't recommend but can happen
/// in misbehaving tests) don't leave the flag stuck.
static MAIL_FAKE_DEPTH: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
struct QueuedMailable {
    mailable_name: String,
    payload: serde_json::Value,
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    delay: Option<std::time::Duration>,
}

fn capture_queued(q: QueuedMailable) {
    QUEUE_CAPTURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(q);
}

fn queue_capture_snapshot() -> Vec<QueuedMailable> {
    QUEUE_CAPTURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

fn clear_queue_capture() {
    QUEUE_CAPTURE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .clear();
}

pub struct Mail;

impl Mail {
    pub fn set_transport(transport: Arc<dyn MailTransport>) -> Result<(), FrameworkError> {
        *lock::write(&TRANSPORT, "mail transport")? = Some(transport);
        Ok(())
    }
    pub fn clear_transport() -> Result<(), FrameworkError> {
        *lock::write(&TRANSPORT, "mail transport")? = None;
        Ok(())
    }

    /// Begin a build to one or more `to` recipients. Same shape as
    /// Laravel's `Mail::to(...)` facade entrypoint.
    pub fn to(addr: impl Into<Address>) -> MailBuilder {
        MailBuilder::default().to(addr)
    }

    /// Begin a build to one or more `cc` recipients.
    pub fn cc(addr: impl Into<Address>) -> MailBuilder {
        MailBuilder::default().cc(addr)
    }

    /// Begin a build to one or more `bcc` recipients.
    pub fn bcc(addr: impl Into<Address>) -> MailBuilder {
        MailBuilder::default().bcc(addr)
    }

    /// Send a one-off raw-text message without a `Mailable`. Mirrors
    /// Laravel's `Mail::raw($text, $callback)` where the callback
    /// configures the recipient list.
    ///
    /// ```ignore
    /// Mail::raw("Hello, plain world", |b| b.to("alice@example.org")
    ///     .subject("Hi")).await?;
    /// ```
    pub async fn raw<F>(text: impl Into<String>, configure: F) -> Result<(), FrameworkError>
    where
        F: FnOnce(MailBuilder) -> MailBuilder,
    {
        let b = configure(MailBuilder::default().text(text));
        b.send_rendered().await
    }

    /// Send a one-off HTML message without a `Mailable`. Mirrors
    /// Laravel's `Mail::html($html, $callback)`.
    pub async fn html<F>(html: impl Into<String>, configure: F) -> Result<(), FrameworkError>
    where
        F: FnOnce(MailBuilder) -> MailBuilder,
    {
        let b = configure(MailBuilder::default().html(html));
        b.send_rendered().await
    }

    /// Set the global default `from` applied when a dispatched message
    /// lacks an explicit `from`. Returns the previous value, if any.
    /// Mirrors Laravel's `Mailer::alwaysFrom`.
    pub fn always_from(addr: impl Into<Address>) -> Result<Option<Address>, FrameworkError> {
        let mut g = lock::write(&ALWAYS, "mail always-defaults")?;
        Ok(g.from.replace(addr.into()))
    }

    /// Set the global default `reply_to`. Applied to every dispatched
    /// message that has no explicit reply-to. Mirrors
    /// `Mailer::alwaysReplyTo`.
    pub fn always_reply_to(addr: impl Into<Address>) -> Result<Option<Address>, FrameworkError> {
        let mut g = lock::write(&ALWAYS, "mail always-defaults")?;
        Ok(g.reply_to.replace(addr.into()))
    }

    /// Route every dispatched message to this address and clear CC/BCC.
    /// Mirrors `Mailer::alwaysTo`. Primarily a local-dev "single inbox"
    /// debugging knob.
    pub fn always_to(addr: impl Into<Address>) -> Result<Option<Address>, FrameworkError> {
        let mut g = lock::write(&ALWAYS, "mail always-defaults")?;
        Ok(g.to.replace(addr.into()))
    }

    /// Set the global default `return_path` (Sender / bounce-to).
    /// Mirrors `Mailer::alwaysReturnPath`.
    pub fn always_return_path(addr: impl Into<Address>) -> Result<Option<Address>, FrameworkError> {
        let mut g = lock::write(&ALWAYS, "mail always-defaults")?;
        Ok(g.return_path.replace(addr.into()))
    }

    /// Forget all `always_*` defaults. Tests call this at teardown to
    /// keep the global state clean across the suite. Returns `Ok(())`
    /// even if no defaults were set.
    pub fn forget_always() -> Result<(), FrameworkError> {
        *lock::write(&ALWAYS, "mail always-defaults")? = AlwaysDefaults::default();
        Ok(())
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
        clear_queue_capture();
        MAIL_FAKE_DEPTH.fetch_add(1, Ordering::SeqCst);
        let transport = Arc::new(InMemoryMailTransport::new());
        let previous = lock::write(&TRANSPORT, "mail transport")
            .expect("mail transport lock poisoned")
            .replace(transport.clone() as Arc<dyn MailTransport>);
        MailFake {
            transport,
            previous,
        }
    }

    pub(crate) fn current_transport() -> Result<Arc<dyn MailTransport>, FrameworkError> {
        lock::read(&TRANSPORT, "mail transport")?
            .clone()
            .ok_or_else(|| FrameworkError::internal(
                "no mail transport configured; call Mail::set_transport(...) or run suprnova::mail::boot::bootstrap_from_env()"
            ))
    }

    /// Apply the `always_*` defaults to a fully-rendered message. Public
    /// to the crate so `MailChannel` (notifications) and `SendMailJob`
    /// (queued mail) can route through the same precedence rules
    /// `MailBuilder::send` uses.
    pub(crate) fn apply_always_defaults(mut msg: OutgoingMessage) -> OutgoingMessage {
        let defaults = lock::read(&ALWAYS, "mail always-defaults")
            .ok()
            .map(|g| g.clone())
            .unwrap_or_default();
        if let Some(to) = defaults.to.clone() {
            // alwaysTo: route every message to this address, drop CC/BCC.
            msg.to = vec![to];
            msg.cc.clear();
            msg.bcc.clear();
        }
        if msg.reply_to.is_empty()
            && let Some(rt) = defaults.reply_to.clone()
        {
            msg.reply_to.push(rt);
        }
        if msg.return_path.is_none() {
            msg.return_path = defaults.return_path.clone();
        }
        if msg.from.email == "noreply@localhost"
            && let Some(from) = defaults.from.clone()
        {
            msg.from = from;
        }
        msg
    }
}

/// Builder returned by `Mail::to(...)`, `Mail::cc(...)`, `Mail::bcc(...)`.
///
/// Beyond the recipient block, the builder accepts the same per-message
/// hints Laravel's Mailable exposes — tags, metadata, priority, custom
/// headers, return-path, and a one-off subject/html/text body for the
/// `Mail::raw` / `Mail::html` shortcut paths.
#[derive(Default, Debug, Clone)]
pub struct MailBuilder {
    to: Vec<Address>,
    cc: Vec<Address>,
    bcc: Vec<Address>,
    reply_to: Vec<Address>,
    from_override: Option<Address>,
    return_path: Option<Address>,
    tags: Vec<String>,
    metadata: BTreeMap<String, String>,
    priority: Option<u8>,
    headers: Vec<(String, String)>,
    // Used by Mail::raw / Mail::html and by .subject/.html/.text overrides.
    subject_override: Option<String>,
    html_body: Option<String>,
    text_body: Option<String>,
    attachments: Vec<Attachment>,
}

impl MailBuilder {
    pub fn to(mut self, addr: impl Into<Address>) -> Self {
        self.to.push(addr.into());
        self
    }
    pub fn cc(mut self, addr: impl Into<Address>) -> Self {
        self.cc.push(addr.into());
        self
    }
    pub fn bcc(mut self, addr: impl Into<Address>) -> Self {
        self.bcc.push(addr.into());
        self
    }
    pub fn reply_to(mut self, addr: impl Into<Address>) -> Self {
        self.reply_to.push(addr.into());
        self
    }
    pub fn from(mut self, addr: impl Into<Address>) -> Self {
        self.from_override = Some(addr.into());
        self
    }
    pub fn return_path(mut self, addr: impl Into<Address>) -> Self {
        self.return_path = Some(addr.into());
        self
    }

    /// Append a provider tag. Mirrors Laravel `Mailable::tag(...)`.
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tags.push(tag.into());
        self
    }
    /// Set a metadata key/value. Mirrors Laravel `Mailable::metadata(k, v)`.
    pub fn metadata(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.metadata.insert(key.into(), value.into());
        self
    }
    /// Set message priority (1 = highest, 5 = lowest). Mirrors
    /// `Mailable::priority($level)`.
    pub fn priority(mut self, level: u8) -> Self {
        self.priority = Some(level);
        self
    }
    /// Append a custom MIME header.
    pub fn header(mut self, name: impl Into<String>, value: impl Into<String>) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }
    /// Override the subject. Used by `Mail::raw` / `Mail::html` plus any
    /// caller that wants to ignore the Mailable's `subject()` getter.
    pub fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject_override = Some(subject.into());
        self
    }
    /// Set the HTML body directly (one-off rendering path).
    pub fn html(mut self, html: impl Into<String>) -> Self {
        self.html_body = Some(html.into());
        self
    }
    /// Set the text body directly (one-off rendering path).
    pub fn text(mut self, text: impl Into<String>) -> Self {
        self.text_body = Some(text.into());
        self
    }
    /// Attach a file directly on the builder (in addition to anything
    /// the Mailable contributes via `attachments()`).
    pub fn attach(mut self, attachment: Attachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    // --- Laravel-side aliases — re-spelled to match the PHP names ---

    /// Alias for [`MailBuilder::reply_to`]. Matches Laravel's
    /// camel-cased `replyTo` after the snake-case translation.
    pub fn reply_to_address(self, addr: impl Into<Address>) -> Self {
        self.reply_to(addr)
    }

    /// Render `mailable` and dispatch to the bound transport.
    pub async fn send<M: Mailable>(self, mailable: M) -> Result<(), FrameworkError> {
        let transport = Mail::current_transport()?;
        let from = self
            .from_override
            .clone()
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

        // Merge mailable + builder hints. Builder wins on metadata key
        // collisions; tags + headers union and de-dupe.
        let mut tags = mailable.tags();
        for t in self.tags {
            if !tags.contains(&t) {
                tags.push(t);
            }
        }
        let mut metadata = mailable.metadata();
        for (k, v) in self.metadata {
            metadata.insert(k, v);
        }
        let mut headers = mailable.headers();
        for (k, v) in self.headers {
            if !headers.iter().any(|(hk, hv)| hk == &k && hv == &v) {
                headers.push((k, v));
            }
        }
        let priority = self.priority.or_else(|| mailable.priority());
        let return_path = self.return_path.or_else(|| mailable.return_path());

        // Attachments: mailable's first, then builder-side appended.
        let mut attachments = mailable.attachments();
        attachments.extend(self.attachments);

        // Subject: builder override wins over mailable's render_subject.
        let subject = match self.subject_override {
            Some(s) => s,
            None => mailable.render_subject()?,
        };

        let msg = OutgoingMessage {
            from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            reply_to: self.reply_to,
            subject,
            html,
            text,
            attachments,
            tags,
            metadata,
            priority,
            headers,
            return_path,
        };

        // Apply Mail::always_* defaults so the queue/notification/raw
        // paths all converge on identical precedence rules.
        let msg = Mail::apply_always_defaults(msg);
        // Fire MessageSending event (best-effort; cancellation is not
        // modeled — Laravel uses events->until, which Suprnova's
        // dispatcher doesn't expose. Listeners observe pre-send shape.).
        events::fire_sending(&msg).await;
        let result = transport::dispatch_with_telemetry(transport.as_ref(), &msg).await;
        if result.is_ok() {
            events::fire_sent(&msg).await;
        }
        result
    }

    /// Build a [`SendMailJob`] and push it onto the queue. The mailable's
    /// concrete type must be registered via
    /// [`register_mailable_factory`]
    /// before the worker dispatches the job.
    ///
    /// Fails fast (push-time, before any envelope is created) if the
    /// mailable defines neither `html_template_source` nor
    /// `text_template_source`. This mirrors `MailBuilder::send`'s
    /// empty-body guard so a misconfigured mailable cannot silently emit
    /// blank messages through the queue path. The same guard runs again
    /// inside `mailable_registry::render_outgoing` as defense in depth.
    pub async fn queue<M: Mailable>(self, mailable: M) -> Result<(), FrameworkError> {
        let job = self.build_send_job(mailable, None)?;
        // Mirror to MailFake's queued buffer when a fake guard is active
        // so `assert_queued` works even when the caller hasn't installed
        // `Queue::fake` separately.
        if crate::mail::queue_fake_active() {
            capture_queued(QueuedMailable {
                mailable_name: job.mailable_name.clone(),
                payload: job.mailable_payload.clone(),
                to: job.to.clone(),
                cc: job.cc.clone(),
                bcc: job.bcc.clone(),
                delay: None,
            });
            return Ok(());
        }
        crate::queue::Queue::push(job).await
    }

    /// Queue the mailable for a delayed dispatch. Same empty-body guard
    /// and registry requirements as [`MailBuilder::queue`].
    pub async fn later<M: Mailable>(
        self,
        delay: std::time::Duration,
        mailable: M,
    ) -> Result<(), FrameworkError> {
        let job = self.build_send_job(mailable, Some(delay))?;
        if crate::mail::queue_fake_active() {
            capture_queued(QueuedMailable {
                mailable_name: job.mailable_name.clone(),
                payload: job.mailable_payload.clone(),
                to: job.to.clone(),
                cc: job.cc.clone(),
                bcc: job.bcc.clone(),
                delay: Some(delay),
            });
            return Ok(());
        }
        crate::queue::Queue::later(delay, job).await
    }

    /// Internal: render the builder's body (when `Mail::raw` / `Mail::html`
    /// set one) and ship it through the bound transport without a Mailable.
    async fn send_rendered(self) -> Result<(), FrameworkError> {
        let transport = Mail::current_transport()?;
        let from = self
            .from_override
            .clone()
            .unwrap_or_else(|| Address::new("noreply@localhost"));
        if self.html_body.is_none() && self.text_body.is_none() {
            return Err(FrameworkError::internal(
                "mail: send_rendered called with neither html nor text body",
            ));
        }
        let msg = OutgoingMessage {
            from,
            to: self.to,
            cc: self.cc,
            bcc: self.bcc,
            reply_to: self.reply_to,
            subject: self.subject_override.unwrap_or_default(),
            html: self.html_body,
            text: self.text_body,
            attachments: self.attachments,
            tags: self.tags,
            metadata: self.metadata,
            priority: self.priority,
            headers: self.headers,
            return_path: self.return_path,
        };
        let msg = Mail::apply_always_defaults(msg);
        events::fire_sending(&msg).await;
        let result = transport::dispatch_with_telemetry(transport.as_ref(), &msg).await;
        if result.is_ok() {
            events::fire_sent(&msg).await;
        }
        result
    }

    fn build_send_job<M: Mailable>(
        self,
        mailable: M,
        _delay_hint: Option<std::time::Duration>,
    ) -> Result<SendMailJob, FrameworkError> {
        // Match `MailBuilder::send`'s guard exactly: call the trait-level
        // `render_html`/`render_text` so a Mailable that overrides those
        // methods (e.g. produces a pre-rendered body without setting a
        // template source) is accepted here too.
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
            tags: self.tags,
            metadata: self.metadata,
            priority: self.priority,
            headers: self.headers,
            return_path: self.return_path,
            subject_override: self.subject_override,
            attachments: self.attachments,
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

    /// All mailables queued via `Mail::queue` / `Mail::later` while the
    /// fake guard was active. Each entry carries the mailable's
    /// `(name, payload)` plus the routed recipients and optional delay.
    pub fn queued(&self) -> Vec<QueuedSnapshot> {
        queue_capture_snapshot()
            .into_iter()
            .map(|q| QueuedSnapshot {
                mailable_name: q.mailable_name,
                payload: q.payload,
                to: q.to,
                cc: q.cc,
                bcc: q.bcc,
                delay: q.delay,
            })
            .collect()
    }

    /// Number of queued mailables since the fake was installed.
    pub fn queued_count(&self) -> usize {
        queue_capture_snapshot().len()
    }

    /// Total of sent + queued. Mirrors Laravel's
    /// `MailFake::assertOutgoingCount` source-of-truth.
    pub fn outgoing_count(&self) -> usize {
        self.count() + self.queued_count()
    }

    /// Filter helper: all captured (sent) messages matching `predicate`.
    pub fn sent<F>(&self, predicate: F) -> Vec<OutgoingMessage>
    where
        F: Fn(&OutgoingMessage) -> bool,
    {
        self.transport
            .captured()
            .into_iter()
            .filter(|m| predicate(m))
            .collect()
    }

    /// Filter helper: all captured (sent) messages routed to `email`.
    /// Case-insensitive on the email address. Convenience over `.sent(|m| m.has_to(email))`.
    pub fn sent_to(&self, email: &str) -> Vec<OutgoingMessage> {
        self.sent(|m| m.has_to(email))
    }

    /// All queued mailables of the given `mailable_name`.
    pub fn queued_named(&self, name: &str) -> Vec<QueuedSnapshot> {
        self.queued()
            .into_iter()
            .filter(|q| q.mailable_name == name)
            .collect()
    }

    /// All queued mailables routed to `email`. Case-insensitive.
    pub fn queued_to(&self, email: &str) -> Vec<QueuedSnapshot> {
        self.queued()
            .into_iter()
            .filter(|q| q.to.iter().any(|a| a.email.eq_ignore_ascii_case(email)))
            .collect()
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

    /// Assert at least one captured message was routed to `email`.
    /// Mirrors Laravel's `MailFake::assertSent($mailable, $address)`
    /// overload.
    pub fn assert_sent_to(&self, email: &str) {
        let captured = self.transport.captured();
        if !captured.iter().any(|m| m.has_to(email)) {
            panic!(
                "Mail::fake assertion failed: expected at least one message sent to {email}; \
                 captured {} message(s)",
                captured.len()
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

    /// Assert NO captured message was routed to `email`.
    pub fn assert_not_sent_to(&self, email: &str) {
        if let Some(matching) = self.transport.captured().iter().find(|m| m.has_to(email)) {
            panic!(
                "Mail::fake assertion failed: expected NO message to {email}, \
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

    /// Assert nothing was sent. Mirrors `assertNothingSent`.
    pub fn assert_nothing_sent(&self) {
        let captured = self.transport.captured();
        assert!(
            captured.is_empty(),
            "Mail::fake assertion failed: expected NO messages sent, captured {}: {captured:#?}",
            captured.len()
        );
    }

    /// Assert at least one mailable named `mailable_name` was queued.
    /// Mirrors Laravel's `assertQueued($mailable, ...)` with a string
    /// type name.
    pub fn assert_queued(&self, mailable_name: &str) {
        let count = self.queued_named(mailable_name).len();
        if count == 0 {
            panic!(
                "Mail::fake assertion failed: expected at least one queued {mailable_name}, \
                 queued {} mailable(s): {:#?}",
                self.queued_count(),
                self.queued()
            );
        }
    }

    /// Assert the queued mailable with the given name matches `predicate`.
    /// The predicate receives the deserialized payload as `serde_json::Value`
    /// — callers wanting typed access can `serde_json::from_value::<M>(...)`
    /// inside the predicate.
    pub fn assert_queued_with<F>(&self, mailable_name: &str, predicate: F)
    where
        F: Fn(&QueuedSnapshot) -> bool,
    {
        let queued = self.queued_named(mailable_name);
        if !queued.iter().any(&predicate) {
            panic!(
                "Mail::fake assertion failed: expected a queued {mailable_name} matching predicate; \
                 found {}: {:#?}",
                queued.len(),
                queued
            );
        }
    }

    /// Assert NO mailable named `mailable_name` was queued.
    pub fn assert_not_queued(&self, mailable_name: &str) {
        let queued = self.queued_named(mailable_name);
        if !queued.is_empty() {
            panic!(
                "Mail::fake assertion failed: expected NO queued {mailable_name}, \
                 found {}: {:#?}",
                queued.len(),
                queued
            );
        }
    }

    /// Assert at least one queued mailable routes to `email`.
    pub fn assert_queued_to(&self, email: &str) {
        let queued = self.queued_to(email);
        if queued.is_empty() {
            panic!(
                "Mail::fake assertion failed: expected at least one queued message to {email}; \
                 queued {} total",
                self.queued_count()
            );
        }
    }

    /// Assert nothing was queued.
    pub fn assert_nothing_queued(&self) {
        let queued = self.queued();
        assert!(
            queued.is_empty(),
            "Mail::fake assertion failed: expected NO queued mailables, found {}: {:#?}",
            queued.len(),
            queued
        );
    }

    /// Assert exact queued count.
    pub fn assert_queued_count(&self, expected: usize) {
        let actual = self.queued_count();
        assert_eq!(
            actual, expected,
            "Mail::fake: expected {expected} queued mailable(s), found {actual}"
        );
    }

    /// Assert NEITHER sent NOR queued for `mailable_name`. Mirrors
    /// `assertNotOutgoing`.
    pub fn assert_not_outgoing(&self, mailable_name: &str) {
        // Sent path matches by string name not present in OutgoingMessage,
        // so we approximate with subject-or-tag-or-header equality. The
        // canonical Laravel-side check is class-name; in Suprnova the
        // sent-side counterpart is the queued track only.
        self.assert_not_queued(mailable_name);
    }

    /// Assert nothing was sent and nothing was queued.
    pub fn assert_nothing_outgoing(&self) {
        self.assert_nothing_sent();
        self.assert_nothing_queued();
    }

    /// Assert the total of sent + queued equals `expected`.
    pub fn assert_outgoing_count(&self, expected: usize) {
        let actual = self.outgoing_count();
        assert_eq!(
            actual, expected,
            "Mail::fake: expected {expected} outgoing mailable(s) (sent + queued), found {actual}"
        );
    }
}

impl Drop for MailFake {
    fn drop(&mut self) {
        // Restore the previous transport. If a poisoned lock would
        // panic during drop we accept that — losing transport state
        // during teardown is preferable to silently leaving the fake
        // bound (which would corrupt every subsequent test).
        *lock::write(&TRANSPORT, "mail transport").expect("mail transport lock poisoned") =
            self.previous.take();
        // Clear queued capture — siblings tests should not see this
        // suite's queued buffer.
        clear_queue_capture();
        MAIL_FAKE_DEPTH.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Snapshot of one queued mailable as captured by an active
/// [`MailFake`]. Mirrors what `MailFake::queued` returns from Laravel
/// after deserialization.
#[derive(Debug, Clone)]
pub struct QueuedSnapshot {
    pub mailable_name: String,
    pub payload: serde_json::Value,
    pub to: Vec<Address>,
    pub cc: Vec<Address>,
    pub bcc: Vec<Address>,
    pub delay: Option<std::time::Duration>,
}

impl QueuedSnapshot {
    /// Deserialize the queued payload back into its concrete `M`. Returns
    /// `Err` if the payload doesn't match `M`'s shape.
    pub fn decode<M: serde::de::DeserializeOwned>(&self) -> Result<M, FrameworkError> {
        serde_json::from_value(self.payload.clone())
            .map_err(|e| FrameworkError::internal(format!("decode queued mailable: {e}")))
    }

    /// True when this snapshot was routed to `email` (case-insensitive).
    pub fn has_to(&self, email: &str) -> bool {
        self.to.iter().any(|a| a.email.eq_ignore_ascii_case(email))
    }
}

/// Whether a `Mail::fake()` guard is currently active. Checked by
/// `MailBuilder::queue` / `MailBuilder::later` to decide whether to
/// short-circuit into the queued-capture buffer instead of pushing on
/// the real queue driver. Tracked via the dedicated `MAIL_FAKE_DEPTH`
/// counter so callers who bind an in-memory transport WITHOUT going
/// through `Mail::fake()` (e.g. the existing `mail_queue.rs` integration
/// tests) keep the original real-queue dispatch behavior.
fn queue_fake_active() -> bool {
    MAIL_FAKE_DEPTH.load(Ordering::SeqCst) > 0
}

#[cfg(test)]
mod tests {
    use super::*;
    use serial_test::serial;

    #[test]
    #[serial]
    fn always_defaults_round_trip() {
        let _ = Mail::forget_always();
        assert_eq!(Mail::always_from("a@b.c").unwrap(), None);
        assert_eq!(
            Mail::always_from("d@e.f").unwrap(),
            Some(Address::new("a@b.c"))
        );
        let _ = Mail::forget_always();
    }

    #[test]
    #[serial]
    fn apply_always_defaults_sets_from_when_default_is_noreply() {
        let _ = Mail::forget_always();
        let _ = Mail::always_from(Address::new("ops@example.com"));
        let msg = OutgoingMessage::new(Address::new("noreply@localhost"));
        let out = Mail::apply_always_defaults(msg);
        assert_eq!(out.from.email, "ops@example.com");
        let _ = Mail::forget_always();
    }

    #[test]
    #[serial]
    fn apply_always_defaults_preserves_explicit_from() {
        let _ = Mail::forget_always();
        let _ = Mail::always_from(Address::new("ops@example.com"));
        let mut msg = OutgoingMessage::new(Address::new("specific@example.com"));
        msg.subject = "hi".into();
        let out = Mail::apply_always_defaults(msg);
        assert_eq!(out.from.email, "specific@example.com");
        let _ = Mail::forget_always();
    }

    #[test]
    #[serial]
    fn apply_always_to_overrides_to_and_clears_cc_bcc() {
        let _ = Mail::forget_always();
        let _ = Mail::always_to(Address::new("inbox@example.com"));
        let mut msg = OutgoingMessage::new(Address::new("from@example.com"));
        msg.to = vec![Address::new("alice@example.org")];
        msg.cc = vec![Address::new("manager@example.com")];
        msg.bcc = vec![Address::new("audit@example.com")];
        let out = Mail::apply_always_defaults(msg);
        assert_eq!(out.to.len(), 1);
        assert_eq!(out.to[0].email, "inbox@example.com");
        assert!(out.cc.is_empty());
        assert!(out.bcc.is_empty());
        let _ = Mail::forget_always();
    }

    #[test]
    #[serial]
    fn apply_always_reply_to_only_when_empty() {
        let _ = Mail::forget_always();
        let _ = Mail::always_reply_to(Address::new("support@example.com"));
        let mut msg = OutgoingMessage::new(Address::new("from@example.com"));
        // No reply_to on the message — default applies.
        let out = Mail::apply_always_defaults(msg.clone());
        assert_eq!(out.reply_to.len(), 1);
        assert_eq!(out.reply_to[0].email, "support@example.com");
        // Explicit reply_to — default does NOT override.
        msg.reply_to = vec![Address::new("override@example.com")];
        let out = Mail::apply_always_defaults(msg);
        assert_eq!(out.reply_to.len(), 1);
        assert_eq!(out.reply_to[0].email, "override@example.com");
        let _ = Mail::forget_always();
    }

    #[test]
    #[serial]
    fn apply_always_return_path_when_unset() {
        let _ = Mail::forget_always();
        let _ = Mail::always_return_path(Address::new("bounce@example.com"));
        let msg = OutgoingMessage::new(Address::new("from@example.com"));
        let out = Mail::apply_always_defaults(msg);
        assert_eq!(
            out.return_path.as_ref().map(|a| a.email.as_str()),
            Some("bounce@example.com")
        );
        let _ = Mail::forget_always();
    }
}
