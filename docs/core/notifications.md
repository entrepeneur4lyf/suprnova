---
title: "Notifications"
description: "Send a single notification across mail, database, web push, and broadcast channels with one Notify::send call"
icon: "bell"
---

# Notifications

Suprnova's notification subsystem mirrors Laravel's `Notification::send($user, $notification)` API: a single call fans out across every channel the notification declares. The recipient (`Notifiable`) addresses each channel — email for `mail`, database id for `database`, push subscription endpoint for `webpush`. Channels are registered with a `NotificationDispatcher`; the `Notify` facade is the call site for both in-process (`Notify::send`) and queued (`Notify::queue`) delivery.

## Quick Start

```rust
use serde::{Deserialize, Serialize};
use suprnova::notifications::{Notifiable, Notification, Notify};
use suprnova::serde_json;
use suprnova::FrameworkError;
use suprnova::NotificationMailable;
use suprnova::mail::Address;
use suprnova::notifications::channels::mail::MailRendering;

#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Order shipped — tracking {{ tracking }}",
    html    = "<p>Your order is on its way.</p><p>Tracking: <code>{{ tracking }}</code></p>",
    text    = "Tracking: {{ tracking }}",
    from    = "orders@example.com",
    from_name = "Acme Orders",
)]
pub struct OrderShipped {
    pub tracking: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str { "OrderShipped" }
    fn channels(&self) -> Vec<&'static str> { vec!["mail", "database"] }
    fn data(&self) -> serde_json::Value {
        serde_json::json!({ "tracking": self.tracking })
    }
}

struct User { id: i64, email: String }
impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "mail" => Some(self.email.clone()),
            "database" => Some(self.id.to_string()),
            _ => None,
        }
    }
}

async fn ship(user: &User, tracking: String) -> Result<(), FrameworkError> {
    Notify::send(user, &OrderShipped { tracking }).await
}
```

One `Notify::send` call dispatches to both the mail channel and the database channel. The recipient declines a channel by returning `None` from `route_for`.

## The Notifiable Trait

A `Notifiable` is anything that can be addressed — a `User` model, an `Order`, a webhook endpoint:

```rust
pub trait Notifiable: Send + Sync {
    fn route_for(&self, channel: &str) -> Option<String>;
}
```

`route_for("mail")` returns the email address, `route_for("database")` returns the entity id as a string, `route_for("webpush")` returns the serialized subscription endpoint. Returning `None` causes the dispatcher to skip that channel for this recipient — useful for "email-only" or "push-only" users.

## The Notification Trait

```rust
pub trait Notification: Serialize + DeserializeOwned + Send + Sync + 'static {
    fn notification_name() -> &'static str where Self: Sized;
    fn channels(&self) -> Vec<&'static str>;
    fn data(&self) -> serde_json::Value;
}
```

| Method | Purpose |
|--------|---------|
| `notification_name()` | Stable identifier persisted by the database channel and used in queue envelopes. |
| `channels(&self)` | Channel names this notification dispatches to. Order doesn't matter; the dispatcher visits each. |
| `data(&self)` | JSON-serializable payload the channels deliver or persist. Typically `serde_json::to_value(self)` of a subset of fields. |

## Channels

### Mail

The mail channel delivers via the bound mail transport. A notification opts in by implementing `NotificationMailable`:

```rust
pub trait NotificationMailable: Notification {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError>;
}
```

`MailRendering` is the rendering envelope — subject (required), html and/or text (at least one required), optional sender + display name, optional cc/bcc/reply_to/attachments. The mail channel assembles an `OutgoingMessage` from this rendering plus the recipient's `route_for("mail")`.

#### Boot wiring

```rust
use suprnova::notifications::channels::mail::{register_mail_renderer, MailChannel};
use suprnova::notifications::{set_dispatcher, NotificationDispatcher};
use std::sync::Arc;

pub fn bootstrap() {
    // One renderer registration per NotificationMailable type.
    suprnova::register_mail_renderer::<OrderShipped>();

    // Build a dispatcher and bind it globally.
    let dispatcher = NotificationDispatcher::new()
        .register_channel(Arc::new(MailChannel::new()));
    set_dispatcher(Arc::new(dispatcher));
}
```

#### `#[derive(NotificationMailable)]`

The derive flattens the per-Notification `impl` into one `#[mail(...)]` attribute. Templates use [Tera](https://keats.github.io/tera/); `self`'s serialized fields are the context.

```rust
#[derive(Serialize, Deserialize, NotificationMailable)]
#[mail(
    subject = "Welcome {{ name }}",
    html_template = "templates/welcome.html",
    text_template = "templates/welcome.txt",
    from = "hello@example.com",
    from_name = "Acme",
    cc = "ops@example.com, support@example.com",
)]
pub struct Welcome { pub name: String }
```

Supported keys:

| Key | Required? | Purpose |
|-----|-----------|---------|
| `subject` | yes | Tera template — rendered with `self` as context. |
| `html` | ‡ | Inline HTML body Tera template. |
| `html_template` | ‡ | Path to an HTML body Tera template (embedded via `include_str!`). |
| `text` | ‡ | Inline plain-text body Tera template. |
| `text_template` | ‡ | Path to a plain-text body Tera template (embedded via `include_str!`). |
| `from` | no | Sender email — overrides the default `noreply@localhost`. |
| `from_name` | no | Display name. Requires `from`. |
| `cc` | no | Comma-separated CC list (`"a@x.com, b@y.com"`). Whitespace and trailing commas are ignored. |
| `bcc` | no | Comma-separated BCC list. |
| `reply_to` | no | Comma-separated Reply-To list. |

‡ At least one body variant must be present. `html` and `html_template` are mutually exclusive; same for `text` and `text_template`.

The derive enforces every invariant at compile time — missing `subject`, empty body, conflicting variants, `from_name` without `from`, and unknown keys all fail to build rather than failing at dispatch.

For attachments (binary payloads) and dynamic per-instance cc lists, hand-implement `NotificationMailable` instead of using the derive.

### Database

The database channel persists each notification into a `notifications` table. Useful for an in-app inbox or audit trail:

```rust
use suprnova::DatabaseChannel;

let dispatcher = NotificationDispatcher::new()
    .register_channel(Arc::new(MailChannel::new()))
    .register_channel(Arc::new(DatabaseChannel::new(db_connection, "users")));
```

The second argument is the recipient's polymorphic type tag — what you'd write in `notifiable_type` when querying back later. The recipient's `route_for("database")` value becomes `notifiable_id`. The migration ships with the framework: `framework/migrations/20260516_create_notifications_table.sql`.

### Web Push

The web push channel ships a payload to a stored push subscription endpoint via VAPID-signed RFC 8030 push messages (ES256, AES128GCM ECE):

```rust
use std::sync::Arc;
use suprnova::web_push::{VapidKey, WebPushClient};
use suprnova::WebPushChannel;

let client = WebPushClient::new(
    VapidKey::from_pem(/* ... */)?,
    "mailto:ops@example.com",
);
let push_channel = WebPushChannel::new(Arc::new(client), 86_400 /* TTL seconds */);
```

The recipient's `route_for("webpush")` returns a serialized `SubscriptionInfo` JSON. 404 / 410 responses (subscription gone) are logged at WARN and treated as success — the dispatcher does not error on a vanished endpoint.

### Broadcast

`BroadcastChannelStub` emits a `tracing::info!` event for each dispatched notification. Phase 7B replaces it with a real WebSocket broadcast over the framework's supervised worker pool. The stub keeps notification surfaces using the broadcast channel name from blowing up before that ships.

## The Dispatcher

```rust
use suprnova::notifications::{NotificationDispatcher, set_dispatcher};
use std::sync::Arc;

let dispatcher = NotificationDispatcher::new()
    .register_channel(Arc::new(MailChannel::new()))
    .register_channel(Arc::new(DatabaseChannel::new(db, "users")))
    .register_channel(Arc::new(WebPushChannel::new(push_client, 86_400)));

set_dispatcher(Arc::new(dispatcher));
```

Channels register by name. Last-write-wins on the channel name makes test setups ergonomic — swap a real channel for a stub in setup. Notifications declaring a channel the dispatcher does not register emit a `tracing::warn!` event and are skipped (the dispatch does not error).

`Notify::send` is the in-process call site:

```rust
Notify::send(&user, &OrderShipped { tracking: "1Z...".into() }).await?;
```

`Notify::queue` builds a `SendNotificationJob` and pushes it onto the Phase 5A queue. The job carries the pre-resolved per-channel routes so the worker doesn't need the recipient at execute time:

```rust
suprnova::notifications::register_notification_factory::<OrderShipped>();
Notify::queue(&user, OrderShipped { tracking }).await?;
```

(Notifications using the database channel need access to the `DatabaseChannel`'s connection at worker time. Bind both via `set_dispatcher` before the worker starts.)

Dispatcher returns on the first channel error; channels that already succeeded are not rolled back. For at-least-once semantics, dispatch each side via the queue — the FROZEN envelope's idempotency keys protect against double-sends on retry.

## Telemetry

`NotificationDispatcher::notify` wraps the channel fan-out in a `notification.dispatch` `tracing::info_span!` carrying:

- `notification` — `Notification::notification_name()`
- `channel_count` — how many channels the notification declared

On completion: `notification dispatched` (info) or `notification dispatch failed` (warn) with `duration_ms`. The mail channel emits its own nested `mail.send` span inside the dispatch span; database and web push channels do not currently emit per-channel spans (Phase 8 territory).

## Testing

Combine `Mail::fake()` with a custom `NotificationDispatcher` for in-process notification tests:

```rust
use serial_test::serial;
use std::sync::Arc;
use suprnova::mail::Mail;
use suprnova::notifications::channels::mail::MailChannel;
use suprnova::notifications::{set_dispatcher, NotificationDispatcher, Notify};

#[tokio::test]
#[serial]
async fn shipping_a_box_dispatches_mail() {
    let fake = Mail::fake();
    suprnova::register_mail_renderer::<OrderShipped>();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new()
            .register_channel(Arc::new(MailChannel::new())),
    ));

    Notify::send(
        &User { id: 1, email: "alice@example.org".into() },
        &OrderShipped { tracking: "1Z...".into() },
    ).await.unwrap();

    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.subject.contains("1Z..."));
}
```

Tests touching the dispatcher / renderer / transport globals must be `#[serial_test::serial]` — those are process-global statics.

## Reference

- Traits: `suprnova::Notifiable`, `suprnova::Notification`, `suprnova::Channel`, `suprnova::DynNotification`
- Facade: `suprnova::Notify`
- Dispatcher: `suprnova::NotificationDispatcher`, `suprnova::notifications::set_dispatcher`
- Channels: `suprnova::MailChannel`, `suprnova::DatabaseChannel`, `suprnova::WebPushChannel`, `suprnova::BroadcastChannelStub`
- Mail channel: `suprnova::notifications::channels::mail::{register_mail_renderer, MailRendering, NotificationMailable}`
- Derive macro: `#[derive(NotificationMailable)]`
- Queue job: `suprnova::SendNotificationJob`
- Factory registry: `suprnova::notifications::register_notification_factory`
