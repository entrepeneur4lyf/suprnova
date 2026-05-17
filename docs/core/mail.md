---
title: "Mail"
description: "Send mail through SMTP or any of five HTTP providers with a unified facade, template-driven Mailables, queueing, and a first-class test fake"
icon: "envelope"
---

# Mail

Suprnova's mail subsystem mirrors Laravel's `Mail::to(...)->send(...)` ergonomics on a Rust-native, Tokio-async runtime. One `Mail` facade, seven first-class transports (log, in-memory, SMTP, plus the five major HTTP providers), Tera-rendered templates with `self` as the context, queue + delayed delivery via the Phase 5A envelope, and a `Mail::fake()` test guard cut from the same cloth as `Bus::fake()` and `Cache::fake()`.

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
| `log`         | Emit a `tracing::info!` per send and discard. Default. |
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

`Mail::queue(...)` builds a `SendMailJob` and pushes it onto the Phase 5A queue. The worker rebuilds the mailable from the registered factory and dispatches through the bound transport:

```rust
// One-time: register every Mailable type the worker will see.
suprnova::mail::register_mailable_factory::<Welcome>();

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

On completion the span emits `mail sent` (info) or `mail send failed` (warn) with `duration_ms`. The same wrapper covers `Mail::send`, the `SendMailJob` queue worker, and the notification `MailChannel`, so the span schema is identical regardless of how the message was produced. (Per-domain attributes layered by Phase 8's admin observability work; these baseline fields stay constant.)

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
suprnova::mail::Mail::set_transport(Arc::new(StdoutTransport));
```

Transports run on Tokio's runtime — async IO, connection pooling, and concurrent send are first-class. There is no per-request fork penalty.

## Best Practices

### Register factories at boot, not per-request

`Mail::queue` and `Mail::later` push a `SendMailJob` carrying the mailable's name and JSON payload — the worker rebuilds the concrete type via `mailable_registry`. Register every queueable `Mailable` once at `Server::serve` time:

```rust
// bootstrap.rs
pub fn register() {
    suprnova::mail::register_mailable_factory::<WelcomeEmail>();
    suprnova::mail::register_mailable_factory::<PasswordReset>();
    suprnova::mail::register_mailable_factory::<InvoiceShipped>();
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

`MailBuilder::send` is at-most-once: if the transport fails halfway through dispatching to two providers, you cannot retry without risking double-send. `MailBuilder::queue` rides the Phase 5A FROZEN envelope, which supports idempotency keys and worker-level retry. For any mail you must not lose AND must not double-send, queue with a stable idempotency key tied to the originating event.

### Test against `Mail::fake()`, not against the bound transport

`Mail::fake()` installs a process-local capture transport for the duration of the RAII guard and restores whatever was bound before. Tests using it do not need to clear globals on every entry/exit — drop semantics handle that. Combine `#[serial_test::serial]` with `Mail::fake()` for tests that mutate the transport global; concurrent tests would clobber each other otherwise.

## Reference

- Trait: `suprnova::mail::Mailable`
- Facade: `suprnova::mail::Mail`
- Bootstrap: `suprnova::mail::boot::bootstrap_from_env()`
- Transports: `LogMailTransport`, `InMemoryMailTransport`, `SmtpMailTransport`, `PostmarkMailTransport`, `SesMailTransport`, `SendGridMailTransport`, `MailgunMailTransport`, `ResendMailTransport`
- Queue job: `suprnova::mail::SendMailJob`
- Test guard: `suprnova::mail::MailFake`
- Telemetry helper: `suprnova::mail::dispatch_with_telemetry`
