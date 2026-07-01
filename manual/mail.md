# Mail

Suprnova's mail subsystem mirrors Laravel's `Mail::to(...)->send(...)` API on Tokio. One `Mail` facade, eight transports (log and in-memory for dev/tests, SMTP, and five HTTP providers — Postmark, SES, SendGrid, Mailgun, Resend), Tera-rendered templates with the Mailable's serialized fields as the context, queue + delayed delivery on the durable at-least-once envelope, and a `Mail::fake()` test guard cut from the same cloth as `Bus::fake()` and `Cache::fake()`.

## Quick Start

```rust
use serde::{Deserialize, Serialize};
use suprnova::async_trait;
use suprnova::mail::{Address, Mail, Mailable};

#[derive(Serialize, Deserialize)]
struct Welcome {
    name: String,
}

#[async_trait]
impl Mailable for Welcome {
    fn mailable_name() -> &'static str { "Welcome" }
    fn subject(&self) -> String { format!("Welcome, {}", self.name) }
    fn text_template_source(&self) -> Option<String> {
        Some("Hi {{ name }}, welcome aboard.".into())
    }
    fn from(&self) -> Option<Address> {
        Some(Address::new("hello@example.com").with_name("Suprnova"))
    }
}

async fn greet(name: String) -> Result<(), suprnova::FrameworkError> {
    Mail::to("alice@example.org")
        .send(Welcome { name })
        .await
}
```

The Mailable serializes to JSON, which becomes the Tera context for the template; every `pub` field is reachable as `{{ field_name }}`.

## Configuration

`Server::serve` calls `suprnova::mail::boot::bootstrap_from_env()` once at startup. It reads `MAIL_DRIVER` and binds the matching transport. Defaults to the `log` driver when unset.

| `MAIL_DRIVER` | Behavior |
|---------------|----------|
| `log`         | Emit a `tracing::info!` per send (envelope + rendered text body, so links in verification/reset mail land in the console) and discard. Default. |
| `memory`      | Capture every message in-process. See `suprnova::mail::boot::captured_in_memory()`. |
| `smtp`        | Connect to an SMTP server (STARTTLS when credentials are set, plain TCP otherwise). |
| `postmark`    | POST JSON to Postmark's `/email` endpoint. |
| `ses`         | POST SigV4-signed requests to Amazon SES `SendEmail`. |
| `sendgrid`    | POST JSON to SendGrid's `/v3/mail/send`. |
| `mailgun`     | POST `application/x-www-form-urlencoded` (or `multipart/form-data` when attachments are present) to Mailgun's `/v3/{domain}/messages`. |
| `resend`      | POST JSON to Resend's `/emails`. |

### Per-driver environment

```env
# SMTP
MAIL_DRIVER=smtp
MAIL_SMTP_HOST=smtp.mailtrap.io
MAIL_SMTP_PORT=587
MAIL_SMTP_USER=...
MAIL_SMTP_PASS=...

# Postmark
MAIL_DRIVER=postmark
MAIL_POSTMARK_TOKEN=...

# Amazon SES
MAIL_DRIVER=ses
MAIL_SES_ACCESS_KEY=...
MAIL_SES_SECRET_KEY=...
MAIL_SES_REGION=us-east-1

# SendGrid
MAIL_DRIVER=sendgrid
MAIL_SENDGRID_API_KEY=...

# Mailgun
MAIL_DRIVER=mailgun
MAIL_MAILGUN_API_KEY=...
MAIL_MAILGUN_DOMAIN=mg.example.com

# Resend
MAIL_DRIVER=resend
MAIL_RESEND_API_KEY=...
```

Each HTTP provider also honors a corresponding `MAIL_<PROVIDER>_ENDPOINT` override that points at a regional URL or a mock server (useful for integration tests against `wiremock`).

### Auth-flow sender: `MAIL_FROM` and `MAIL_FROM_NAME`

The built-in auth-flow mailables — email verification, password reset, and the
password-changed notice — resolve their envelope `From` from the environment
rather than a hard-coded `from()`:

```env
MAIL_FROM=no-reply@example.com        # bare address (required by the auth flows; fails closed if unset)
MAIL_FROM_NAME=Acme Support           # optional display name (since 0.5.9)
```

- `MAIL_FROM` **must be a bare address.** It is lifted straight into the
  message's `From`, so a `"Name <addr>"` value would be treated as the entire
  address and rejected by the transport.
- `MAIL_FROM_NAME` (optional, added in **0.5.9**) attaches a display name, so the
  header renders as `Acme Support <no-reply@example.com>`. Unset or blank keeps
  the previous bare-address behavior. It is read at send time, so it applies to
  queued auth-flow mail too.

These two variables only affect the framework's own auth-flow mailables. Your
own `Mailable`s set their sender through `from()` (or the global `always_from`
default) — see below.

## The Mailable Trait

Mailables are serializable structs that know how to render themselves. The trait defaults render with `tera::Tera::one_off` against the mailable's serialized fields:

```rust
use suprnova::async_trait;
use suprnova::mail::{Address, Attachment, Mailable};

#[async_trait]
impl Mailable for OrderShipped {
    fn mailable_name() -> &'static str { "OrderShipped" }
    fn subject(&self) -> String {
        format!("Order #{} shipped", self.order_id)
    }
    fn html_template_source(&self) -> Option<String> {
        Some("<p>Tracking: <code>{{ tracking }}</code></p>".into())
    }
    fn text_template_source(&self) -> Option<String> {
        Some("Tracking: {{ tracking }}".into())
    }
    fn from(&self) -> Option<Address> {
        Some(Address::new("orders@example.com").with_name("Acme Orders"))
    }
    fn attachments(&self) -> Vec<Attachment> {
        vec![Attachment::new("invoice.pdf", self.invoice_bytes.clone(), "application/pdf")]
    }
}
```

| Method | Required? | Purpose |
|--------|-----------|---------|
| `mailable_name()` | yes | Stable name persisted in the queue envelope — renaming breaks in-flight queued mail. |
| `subject(&self)` | yes | Computed subject. Used verbatim when `subject_template_source` returns `None`. |
| `subject_template_source(&self)` | optional | Tera template for the subject — when `Some`, takes precedence over `subject()` and renders with `self` as the context. Same semantics as the body template sources. |
| `html_template_source(&self)` | optional | HTML body Tera template. Return `None` to skip HTML. |
| `text_template_source(&self)` | optional | Plain-text body Tera template. Return `None` to skip text. |
| `from(&self)` | optional | Override the global default `noreply@localhost`. |
| `attachments(&self)` | optional | Files to attach. Each is `name + bytes + mime`. |
| `render_subject(&self)` / `render_html(&self)` / `render_text(&self)` | optional | Override if you want to bypass Tera (Markdown → HTML, pre-rendered content, custom subject logic, etc.). |

At least one of `html_template_source` or `text_template_source` must return `Some` (or `render_html`/`render_text` must produce content). An empty-body mailable is refused both at dispatch (`Mail::send`) and at enqueue (`Mail::queue`).

### Tera autoescape

Autoescape is **OFF** because mail bodies are typically hand-authored HTML where Tera's `<>&` escaping would over-escape. If your literal body contains `{{` for non-template reasons (e.g., marketing copy quoting Mustache syntax), escape it: `{% raw %}{{ literal }}{% endraw %}`.

## Building Messages

The `Mail::to(...)` builder threads recipients, CC/BCC, reply-to, and a per-message sender override into the dispatch:

```rust
Mail::to("alice@example.org")
    .cc("manager@example.com")
    .bcc("audit@example.com")
    .reply_to("support@example.com")
    .from(("Operations", "ops@example.com"))   // (display name, email)
    .send(OrderShipped { order_id: 42, /* ... */ })
    .await?;
```

`Address` accepts `&str`, `String`, and `(name, email)` tuples; `Mail::to(...)` accepts anything `Into<Address>`.

## Attachments

```rust
use suprnova::mail::Attachment;

let attachment = Attachment::new(
    "report.csv",
    csv_bytes,
    "text/csv",
);
```

Attachments ride through the `Mailable::attachments` method. All five HTTP providers handle them — Postmark/SendGrid/Resend over JSON (base64-encoded), SES via Raw MIME (since `Content.Simple` does not support attachments), and Mailgun via `multipart/form-data` (the form-encoded path is used when there are no attachments).

## Queueing

`Mail::queue(...)` builds a `SendMailJob` and pushes it onto the framework queue. The worker rebuilds the mailable from the registered factory and dispatches through the bound transport:

```rust
// One-time: register every Mailable type the worker will see.
suprnova::mail::register_mailable_factory::<Welcome>()?;

// At send time:
Mail::to("alice@example.org").queue(Welcome { name: "Alice".into() }).await?;

// Delayed:
use std::time::Duration;
Mail::to("alice@example.org")
    .later(Duration::from_secs(60), Welcome { name: "Alice".into() })
    .await?;
```

The same empty-body guard runs on the queue path, so a misconfigured Mailable is rejected at push-time before any envelope is created.

## Telemetry

Every send routes through `suprnova::mail::dispatch_with_telemetry`, which opens a `mail.send` `tracing::info_span!` carrying:

- `transport` — driver name (`"postmark"`, `"smtp"`, `"in-memory"`, …)
- `to_count`, `cc_count`, `bcc_count` — recipient counts
- `has_html`, `has_text` — body shape
- `attachment_count` — number of attachments
- `tag_count`, `metadata_count` — provider-hint counts
- `priority` — `1..=5`, or `0` when unset

On completion the span emits `mail sent` (info) or `mail send failed` (warn) with `duration_ms`. The same wrapper covers `Mail::send`, the `SendMailJob` queue worker, and the notification `MailChannel`, so the span schema is identical regardless of how the message was produced.

## Testing with `Mail::fake()`

`Mail::fake()` installs an in-memory capture transport for the duration of the returned RAII guard. Mirrors `Bus::fake()` / `Queue::fake()` / `Cache::fake()`:

```rust
use suprnova::mail::Mail;

#[tokio::test]
async fn welcome_mail_is_sent_on_signup() {
    let fake = Mail::fake();

    sign_up("alice@example.org").await.unwrap();

    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.to.iter().any(|a| a.email == "alice@example.org"));
    fake.assert_sent(|m| m.subject.starts_with("Welcome"));
    fake.assert_not_sent(|m| m.subject.contains("Password reset"));
}
```

When the guard drops, the previously-bound transport (if any) is restored. Tests that intermix `Mail::fake()` with explicit transport binding do not leak state.

`Mail::fake()` is `Send + Sync`; share it across awaits or threads as needed.

## Custom Transports

The `MailTransport` trait is the integration point:

```rust
use suprnova::async_trait;
use suprnova::mail::{MailTransport, OutgoingMessage};
use suprnova::FrameworkError;

pub struct StdoutTransport;

#[async_trait]
impl MailTransport for StdoutTransport {
    async fn send(&self, msg: &OutgoingMessage) -> Result<(), FrameworkError> {
        println!("--- mail ---\n{}\n--- end ---", msg.subject);
        Ok(())
    }
    fn name(&self) -> &'static str { "stdout" }
}

// At boot:
use std::sync::Arc;
suprnova::mail::Mail::set_transport(Arc::new(StdoutTransport))?;
```

Transports run on Tokio's runtime — async IO, connection pooling, and concurrent send are first-class. There is no per-request fork penalty.

### Why Suprnova diverges

Laravel's Mailable layer is built on Symfony Mailer, which runs synchronously inside the request lifecycle. Suprnova's `MailTransport` is `async fn send(&self, msg: &OutgoingMessage)` end-to-end: the HTTP providers use `reqwest`, the SMTP path uses an async lettre adapter, and `dispatch_with_telemetry` wraps every send in a Tokio `tracing` span. Long-haul providers don't block the handler thread, connection pools survive across requests, and concurrent sends in one handler are trivial — `tokio::try_join!(Mail::to(a).send(m), Mail::to(b).send(n))` does what you'd expect.

The other divergence is event cancellation. Laravel models a `MessageSending` listener that can return `false` and suppress the send (`events->until()`). Suprnova's dispatcher does not expose a short-circuit return channel — `MessageSending` is observation-only. To gate a send, refuse at the Mailable layer (override `render_html` / `render_text` to return an error) or wrap the `MailBuilder::send` call with your own guard. The trade is real: we lose one Laravel hook to keep the dispatcher's contract simple.

## Best Practices

### Register factories at boot, not per-request

`Mail::queue` and `Mail::later` push a `SendMailJob` carrying the mailable's name and JSON payload — the worker rebuilds the concrete type via `mailable_registry`. Register every queueable `Mailable` once at `Server::serve` time:

```rust
// bootstrap.rs
pub fn register() -> Result<(), suprnova::FrameworkError> {
    suprnova::mail::register_mailable_factory::<WelcomeEmail>()?;
    suprnova::mail::register_mailable_factory::<PasswordReset>()?;
    suprnova::mail::register_mailable_factory::<InvoiceShipped>()?;
    Ok(())
}
```

A `Mail::queue` for an unregistered mailable lands on the queue, runs once, hits "unknown mailable", retries per the envelope's backoff policy, and dead-letters — costing observability time you would not have spent if the factory was bound at boot.

### Queue mail for any slow or unreliable render

Sending mail in a request handler couples the user's response latency to your SMTP server (or whichever provider's HTTP API). Use `Mail::queue` for anything beyond a synchronous local-dev render, and `Mail::later` when you want the dispatch deferred — onboarding follow-ups, reminder emails, scheduled digests.

```rust
// Bad: ties response time to the mail provider
Mail::to(&user.email).send(Welcome { ... }).await?;
return json_response!({ "ok": true });

// Good: 200 OK returns immediately; the worker delivers the mail.
Mail::to(&user.email).queue(Welcome { ... }).await?;
return json_response!({ "ok": true });
```

### Always set `from` on a Mailable

The framework's default sender is `noreply@localhost` — useful for catching missing senders in development, not a sender any provider will accept in production. Override `Mailable::from(&self)` (or set `from = "..."` in the `#[mail(...)]` attribute on a `NotificationMailable`) so every dispatched message has a real sender identity:

```rust
fn from(&self) -> Option<Address> {
    Some(Address::new("orders@example.com").with_name("Acme Orders"))
}
```

The per-message override on `MailBuilder` (`.from(("Operations", "ops@example.com"))`) takes precedence over the mailable's default — useful for one-off transactional sends.

### Use the queue for at-least-once delivery, not the direct path

`MailBuilder::send` is at-most-once: if the transport fails halfway through dispatching to two providers, you cannot retry without risking double-send. `MailBuilder::queue` rides the durable queue envelope, which supports idempotency keys and worker-level retry. For any mail you must not lose AND must not double-send, queue with a stable idempotency key tied to the originating event.

## One-off Messages: `Mail::raw` and `Mail::html`

When the mail is a single transactional ping that doesn't justify a full `Mailable` struct, two shortcuts skip the boilerplate:

```rust
use suprnova::mail::Mail;

// Plain text
Mail::raw("Your code is 12345", |b| {
    b.to("alice@example.org")
        .subject("Verification code")
        .from("auth@example.com")
}).await?;

// HTML
Mail::html("<p>Hello, <b>world</b></p>", |b| {
    b.to("alice@example.org")
        .subject("Hi")
        .from("hello@example.com")
}).await?;
```

The closure receives a [`MailBuilder`] preloaded with the body and lets you layer recipients, subject, sender, tags, metadata, priority, and any other [`MailBuilder`] fluent method on top. These paths bypass the `Mailable` trait entirely — useful for one-shot test pings and short transactional notes.

## Global Defaults: `always_from`, `always_reply_to`, `always_to`, `always_return_path`

Mirroring Laravel's `Mailer::alwaysFrom` / `alwaysReplyTo` / `alwaysTo` / `alwaysReturnPath`, the Mail facade exposes four global setters:

```rust
use suprnova::mail::{Address, Mail};

// At boot:
Mail::always_from(Address::new("noreply@example.com").with_name("Acme"))?;
Mail::always_reply_to(Address::new("support@example.com"))?;
Mail::always_return_path(Address::new("bounce@example.com"))?;

// Local-dev "single inbox" — route ALL mail to one address, drop CC/BCC:
Mail::always_to(Address::new("dev-inbox@example.com"))?;

// Roll everything back (tests typically call this at teardown):
Mail::forget_always()?;
```

Precedence is conservative — defaults only apply when the dispatched message lacks an explicit value:

| Field | Default applies when |
|-------|---------------------|
| `always_from` | Message `from` is the framework default `noreply@localhost` |
| `always_reply_to` | Message has no explicit `reply_to` |
| `always_to` | Always — routes every message to this address, clears CC/BCC |
| `always_return_path` | Message has no explicit `return_path` |

The same precedence applies on the queue path: queued mailables go through `apply_always_defaults` at worker dispatch time, so direct sends and queued sends converge on identical envelope shapes.

## Tags, Metadata, Priority, Headers, Return-Path

Every dispatched message can carry Laravel-style provider hints — tags, metadata key/values, RFC-2076 priority, custom MIME headers, and a Sender / bounce-to address. They forward to the HTTP providers' native fields (Postmark `Tag` / `Metadata` / `Headers`, SES `EmailTags`, SendGrid `categories` / `custom_args` / `headers`, Mailgun `o:tag` / `v:` / `h:`, Resend `tags` / `headers`) and to SMTP as RFC 5322 headers.

Two ways to attach them — at the Mailable level for per-type defaults, or per-message on the builder:

```rust
use suprnova::async_trait;
use suprnova::mail::{Mailable, PRIORITY_HIGH};
use std::collections::BTreeMap;

#[async_trait]
impl Mailable for OrderShipped {
    fn mailable_name() -> &'static str { "OrderShipped" }
    fn subject(&self) -> String { format!("Order #{} shipped", self.order_id) }
    fn text_template_source(&self) -> Option<String> { Some("...".into()) }

    fn tags(&self) -> Vec<String> { vec!["transactional".into(), "order".into()] }
    fn metadata(&self) -> BTreeMap<String, String> {
        let mut m = BTreeMap::new();
        m.insert("order_id".into(), self.order_id.to_string());
        m
    }
    fn priority(&self) -> Option<u8> { Some(PRIORITY_HIGH) }
    fn headers(&self) -> Vec<(String, String)> {
        vec![("X-Origin".into(), "warehouse".into())]
    }
}
```

```rust
// Per-message on the builder. Builder wins on metadata-key collisions; tags + headers union.
Mail::to(&user.email)
    .tag("campaign-spring")
    .metadata("ab_variant", "B")
    .priority(1)
    .header("X-Source", "promo-feed")
    .return_path("bounce@example.com")
    .send(WelcomeEmail { name: user.name.clone() })
    .await?;
```

Constants for the five priority levels live at `suprnova::mail::{PRIORITY_HIGHEST, PRIORITY_HIGH, PRIORITY_NORMAL, PRIORITY_LOW, PRIORITY_LOWEST}` — same `1..=5` integer scale Laravel uses.

## Inspecting Captured Messages

`OutgoingMessage` carries Laravel-style inspection helpers — useful for both test assertions and runtime audit logging:

```rust
fn audit_outgoing(m: &suprnova::mail::OutgoingMessage) {
    if m.has_tag("transactional") && m.has_to("alice@example.org") { /* ... */ }
    if m.has_metadata("order_id") { /* ... */ }
    if m.has_subject("Welcome") { /* ... */ }
    if m.has_attachment("invoice.pdf") { /* ... */ }
    if m.has_header("X-Source", "promo-feed") { /* ... */ }
}
```

Recipient checks are case-insensitive on email; metadata, tag, subject, and attachment-filename checks are exact.

## Test Fake: Expanded Surface

`Mail::fake()` covers BOTH the sent and queued tracks. Sent mail (via `MailBuilder::send`) lands in the in-memory transport; queued mail (via `.queue` / `.later`) lands in the fake's queue buffer.

```rust
use suprnova::mail::Mail;

#[tokio::test]
async fn boot_dispatches_welcome() {
    let fake = Mail::fake();

    onboard_user("alice@example.org").await.unwrap();

    // Sent-side
    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.has_to("alice@example.org") && m.subject.starts_with("Welcome"));
    fake.assert_sent_to("alice@example.org");
    fake.assert_not_sent(|m| m.subject.contains("Password reset"));

    // Queued-side (for delayed mails)
    fake.assert_queued("WelcomeFollowup");
    fake.assert_queued_to("alice@example.org");
    fake.assert_queued_count(1);

    // Composite
    fake.assert_outgoing_count(2);   // sent + queued
    fake.assert_not_outgoing("PasswordReset");
}
```

Additional helpers:

| Helper | Purpose |
|--------|---------|
| `fake.captured()` | All sent messages |
| `fake.count()` | Sent count |
| `fake.queued()` | All queued `QueuedSnapshot`s |
| `fake.queued_count()` | Queued count |
| `fake.outgoing_count()` | Sent + queued |
| `fake.sent(predicate)` | Filter sent by predicate |
| `fake.sent_to(email)` | Filter sent by recipient |
| `fake.queued_named(name)` | Queued mailables of a given name |
| `fake.queued_to(email)` | Queued mailables to recipient |
| `fake.assert_sent_count(n)` | Exact sent count |
| `fake.assert_queued_count(n)` | Exact queued count |
| `fake.assert_outgoing_count(n)` | Exact total |
| `fake.assert_nothing_sent()` | Empty sent buffer |
| `fake.assert_nothing_queued()` | Empty queued buffer |
| `fake.assert_nothing_outgoing()` | Both empty |
| `fake.assert_sent_to(email)` | At least one sent to recipient |
| `fake.assert_not_sent_to(email)` | None sent to recipient |
| `fake.assert_queued(name)` | At least one queued of name |
| `fake.assert_queued_with(name, fn)` | At least one queued of name matching predicate |
| `fake.assert_queued_to(email)` | At least one queued to recipient |
| `fake.assert_not_queued(name)` | None queued of name |

`QueuedSnapshot::decode::<M>()` deserializes the payload back into the concrete `M`, so type-checked predicates work without bespoke decode boilerplate.

## Events: `MessageSending` and `MessageSent`

Every successful dispatch fires two framework events:

- `MessageSending` — immediately BEFORE the transport call. Listeners observe the message shape (recipients, subject, tags, body-shape flags).
- `MessageSent` — immediately AFTER a successful transport call. Listeners observe the same shape; failed sends do not emit this event.

```rust
use std::sync::Arc;
use suprnova::events::EventFacade;
use suprnova::mail::MessageSent;

EventFacade::listen::<MessageSent, _>(Arc::new(MyAuditListener)).await;
```

Both events are observation-only — the dispatcher does not model a Laravel-style cancellation channel. See [Why Suprnova diverges](#why-suprnova-diverges) above for the gating workaround.

## Multi-recipient Convenience: `Mail::cc` and `Mail::bcc`

The Mail facade exposes three entry points — `to`, `cc`, `bcc` — that all return a fresh `MailBuilder`. Use whichever matches the dominant routing intent:

```rust
// Start with a cc / bcc when the message is primarily an audit copy.
Mail::cc("manager@example.com")
    .to("alice@example.org")
    .send(OrderShipped { /* ... */ })
    .await?;
```

The same fluent surface applies regardless of which entry point you start with.

### Test against `Mail::fake()`, not against the bound transport

`Mail::fake()` installs a process-local capture transport for the duration of the RAII guard and restores whatever was bound before. Tests using it do not need to clear globals on every entry/exit — drop semantics handle that. Combine `#[serial_test::serial]` with `Mail::fake()` for tests that mutate the transport global; concurrent tests would clobber each other otherwise.

## Next

- [Notifications](notifications.md) — `Notify::send` fans out across mail, database, and webpush channels; `#[derive(NotificationMailable)]` is the macro-driven shortcut over the `Mailable` trait
- [Queues](queues.md) — the durable envelope `Mail::queue` and `Mail::later` ride on
- [Events](events.md) — listening for `MessageSending` / `MessageSent` plus the wider dispatcher model
- [Testing](testing.md) — `Mail::fake()` alongside the other `*::fake()` guards
- [Configuration](configuration.md) — typed config registration for service credentials

## Reference

- Trait: `suprnova::mail::Mailable`
- Facade: `suprnova::mail::Mail`
- Bootstrap: `suprnova::mail::boot::bootstrap_from_env()`
- Transports: `LogMailTransport`, `InMemoryMailTransport`, `SmtpMailTransport`, `PostmarkMailTransport`, `SesMailTransport`, `SendGridMailTransport`, `MailgunMailTransport`, `ResendMailTransport`
- Queue job: `suprnova::mail::SendMailJob`
- Test guard: `suprnova::mail::MailFake`
- Telemetry helper: `suprnova::mail::dispatch_with_telemetry`
