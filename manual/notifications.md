# Notifications

A notification is a small message you want a user (or "anyone with an
email address") to receive across one or more channels — mail, in-app
inbox, browser push, real-time WebSocket — from one call site. You
write `Notify::send(&user, &OrderShipped { … })`; the dispatcher fans
that single notification out across every channel the notification
declared, addressing each one through the recipient.

Use notifications when the *what* (an order shipped, an invoice was
paid) is more interesting to your code than the *how* (which transport
ended up delivering it). For raw transport access — composing a custom
mail body, publishing to a specific broadcast channel, sending a one-off
web push — go through [mail](mail.md), [broadcasting](broadcasting.md),
or [web push](web-push.md) directly.

## Quick start

```rust
use serde::{Deserialize, Serialize};
use suprnova::FrameworkError;
use suprnova::NotificationMailable;          // derive macro
use suprnova::notifications::channels::mail::MailRendering;
use suprnova::{Notifiable, Notification, Notify};

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
            "mail"     => Some(self.email.clone()),
            "database" => Some(self.id.to_string()),
            _          => None,
        }
    }
}

async fn ship(user: &User, tracking: String) -> Result<(), FrameworkError> {
    Notify::send(user, &OrderShipped { tracking }).await
}
```

`Notify::send` dispatches to both the mail channel and the database
channel in one call. The recipient declines a channel by returning
`None` from `route_for` — useful for "email-only" or "push-only" users.

## The three traits

| Trait | What it represents | Implemented by |
|---|---|---|
| `Notification` | A typed message + the channels it dispatches to | Your notification structs |
| `Notifiable` | A recipient — exposes a per-channel `route_for` | Your `User`, `Order`, anything addressable |
| `Channel` | A transport — knows how to deliver to a route | Built-in: `MailChannel`, `DatabaseChannel`, `BroadcastChannel`, `WebPushChannel` |

### `Notifiable`

```rust
pub trait Notifiable: Send + Sync {
    fn route_for(&self, channel: &str) -> Option<String>;
}
```

The recipient owns the per-channel addressing. `route_for("mail")`
returns the email address; `route_for("database")` returns the entity
id as a string; `route_for("webpush")` returns a serialized
`SubscriptionInfo` JSON; `route_for("broadcast")` returns the
broadcast channel name. Return `None` to skip a channel for this
recipient.

### `Notification`

```rust
pub trait Notification: Serialize + DeserializeOwned + Send + Sync + 'static {
    fn notification_name() -> &'static str where Self: Sized;
    fn channels(&self) -> Vec<&'static str>;
    fn data(&self) -> serde_json::Value;

    fn should_send(&self, _channel: &str) -> bool { true }
    fn after_sending(&self, _channel: &str) -> Result<(), FrameworkError> { Ok(()) }
}
```

| Method | Purpose |
|---|---|
| `notification_name()` | Stable identifier persisted by the database channel, used as the queue envelope key, and the lookup key for the mail renderer registry. |
| `channels(&self)` | Channel names this notification dispatches to. Order is iteration order. |
| `data(&self)` | JSON-serializable payload channels deliver / persist. Typically `serde_json::to_value(self)` of the subset of fields the channels need. |
| `should_send(&self, channel)` | Per-channel veto consulted by `Notify::send`. Returning `false` skips that channel for this dispatch. Default: always send. |
| `after_sending(&self, channel)` | Post-success hook invoked once per channel that completed (synchronous `Notify::send` only). Returning `Err` propagates the same way a channel error would. Default: no-op. |

`should_send` and `after_sending` fire only on the synchronous path
(`Notify::send` → dispatcher). The queued path executes channels
directly without consulting them — if you rely on the hooks, send
synchronously or perform the equivalent check inside the channel
itself.

## Channels

### Mail

The mail channel delivers via the bound mail transport (see
[Mail](mail.md)). A notification opts in by implementing
`NotificationMailable`:

```rust
pub trait NotificationMailable: Notification {
    fn to_mail(&self) -> Result<MailRendering, FrameworkError>;
}
```

`MailRendering` is the rendering envelope — `subject` (required), `html`
and/or `text` (at least one required), optional `from`, `cc`, `bcc`,
`reply_to`, and `attachments`. The mail channel assembles an outgoing
message from this rendering plus the recipient's `route_for("mail")`,
applies the configured sender defaults (`Mail::always_from(...)`,
`always_to(...)`, etc.), and dispatches through `Mail::current_transport`.

If the renderer returns a rendering with neither `html` nor `text`,
delivery fails fast — blank notification mail is never sent silently.

#### `#[derive(NotificationMailable)]`

The derive collapses the per-Notification `to_mail` `impl` into one
`#[mail(...)]` attribute. Templates use [Tera](https://keats.github.io/tera/);
`self`'s serialized fields are the context.

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
|---|---|---|
| `subject` | yes | Tera template — rendered with `self` as context. |
| `html` | dagger | Inline HTML body Tera template. |
| `html_template` | dagger | Path to an HTML body Tera template (embedded via `include_str!`). |
| `text` | dagger | Inline plain-text body Tera template. |
| `text_template` | dagger | Path to a plain-text body Tera template (embedded via `include_str!`). |
| `from` | no | Sender email — overrides the default `noreply@localhost`. |
| `from_name` | no | Display name. Requires `from`. |
| `cc` | no | Comma-separated CC list. Whitespace and trailing commas ignored. |
| `bcc` | no | Comma-separated BCC list. |
| `reply_to` | no | Comma-separated Reply-To list. |

(dagger) At least one body variant must be present. `html` and
`html_template` are mutually exclusive; same for `text` and
`text_template`.

Every invariant is enforced at compile time — missing `subject`, empty
body, conflicting variants, `from_name` without `from`, or unknown keys
fail the build instead of failing at dispatch.

For attachments (binary payloads) or per-instance dynamic recipients,
hand-implement `NotificationMailable` and build the `MailRendering`
directly.

### Database

The database channel persists each notification as one row in the
`notifications` table:

```rust
use std::sync::Arc;
use suprnova::{DatabaseChannel, NotificationDispatcher};

let dispatcher = NotificationDispatcher::new()
    .register_channel(Arc::new(DatabaseChannel::new(db, "users")));
```

The second argument is the recipient's polymorphic type tag (what you
store in `notifiable_type` so you can query inbox rows back later). The
recipient's `route_for("database")` becomes the `notifiable_id`. The
migration ships with the framework
(`framework/migrations/20260516_create_notifications_table.sql`); run
`suprnova migrate` and the table appears.

#### Reading the inbox

The read-side helpers live in `suprnova::notifications` as free
functions over `(notifiable_type, notifiable_id)`:

```rust
use suprnova::notifications::{
    all_for, unread_for, read_for,
    mark_as_read, mark_as_unread, mark_all_as_read,
    delete_for, StoredNotification,
};

let unread: Vec<StoredNotification> = unread_for(&db, "users", "42").await?;
let count = mark_all_as_read(&db, "users", "42").await?;
let removed = delete_for(&db, "users", "42").await?;
```

`StoredNotification` carries `id`, `type_name` (the
`Notification::notification_name`), `notifiable_type`, `notifiable_id`,
the decoded JSON `data`, `read_at`, `created_at`, `updated_at`.
`mark_as_read` / `mark_as_unread` are idempotent (matching Laravel's
contract).

### Web push

The web push channel encrypts the payload and POSTs it to a stored
browser push subscription endpoint via the framework's
VAPID-signing client:

```rust
use std::sync::Arc;
use suprnova::WebPushChannel;
use suprnova::web_push::{VapidKey, WebPushClient};

let client = WebPushClient::new(
    VapidKey::from_pem(b"-----BEGIN PRIVATE KEY-----\n…")?,
    "mailto:ops@example.com",
)?;
let push_channel = WebPushChannel::new(Arc::new(client), 86_400 /* TTL seconds */);
```

The recipient's `route_for("webpush")` returns a serialized
`SubscriptionInfo` JSON (the same shape the browser hands back from
`PushSubscription.toJSON()` — store it verbatim, return it untouched).
The TTL is forwarded to the push service.

When the push service tells the channel a subscription is gone (HTTP
404/410), the channel logs a structured WARN and returns success — the
notification has reached a terminal state with no recipient to retry
against. Operators see the log and remove the dead subscription;
delivery does not error.

See [Web Push](web-push.md) for the full client.

### Broadcast

The broadcast channel publishes each notification to the application's
`BroadcastHub` so WebSocket subscribers receive it in real time. The
recipient's `route_for("broadcast")` is the channel name, the
notification type is the event, and `data()` is the payload:

```rust
use std::sync::Arc;
use suprnova::BroadcastChannel;
use suprnova::broadcasting::BroadcastHub;
use suprnova::container::App;

// At boot — bind the hub before any broadcast dispatch.
App::bind::<dyn BroadcastHub>(Arc::clone(&hub));

let dispatcher = suprnova::NotificationDispatcher::new()
    .register_channel(Arc::new(BroadcastChannel::new()));
```

The channel resolves the hub from the container at delivery time. If
no `BroadcastHub` is bound when a notification declares `"broadcast"`,
the channel returns an error — a misconfigured application surfaces
the problem instead of silently dropping the message. Publishing to a
channel with zero live subscribers is not an error.

See [Broadcasting](broadcasting.md) for hub setup and WebSocket
plumbing.

## On-demand notifications

Sometimes you want to notify *somebody who isn't in your database* — a
one-off ops alert to an email address, a webhook receiver, a broadcast
channel that no user owns. `AnonymousNotifiable` is the "user without a
row":

```rust
use suprnova::Notify;

let recipient = Notify::route("mail", "ops@example.com")?;
Notify::send(&recipient, &IncidentNotification { id: 7 }).await?;

// Multiple channels in one builder:
let recipient = Notify::routes([
    ("mail", "ops@example.com"),
    ("broadcast", "ops-channel"),
])?;
Notify::send(&recipient, &IncidentNotification { id: 7 }).await?;
```

`Notify::route("database", …)` and `Notify::routes([..., ("database",
…)])` return `Err` — the database channel persists a
`(notifiable_type, notifiable_id)` pair that an anonymous recipient
cannot supply.

## The dispatcher

`NotificationDispatcher` holds the channel registry. Build it once at
boot and bind it globally:

```rust
use std::sync::Arc;
use suprnova::{DatabaseChannel, MailChannel, NotificationDispatcher, WebPushChannel};
use suprnova::notifications::set_dispatcher;

let dispatcher = NotificationDispatcher::new()
    .register_channel(Arc::new(MailChannel::new()))
    .register_channel(Arc::new(DatabaseChannel::new(db, "users")))
    .register_channel(Arc::new(WebPushChannel::new(push_client, 86_400)));

set_dispatcher(Arc::new(dispatcher))?;
```

`register_channel` is last-write-wins on the channel name — registering
two channels named `"mail"` silently replaces the first. This makes
test setups ergonomic.

A notification declaring a channel the dispatcher does not register
logs a WARN (`no channel registered; skipping`) and continues to the
next channel — dispatch does not error on an unknown channel name.

`set_dispatcher` returns `Result<(), FrameworkError>` because the
dispatcher registry lives behind a `RwLock`; the error path triggers
only if the lock is poisoned (a previous writer panicked). In practice
the call site at boot uses `?`.

### Lifecycle events

Three events surround every synchronous channel delivery:

| Event | When | Listener-error behaviour |
|---|---|---|
| `NotificationSending` | Immediately before the channel runs | Listener `Err` **vetoes** the channel for this dispatch |
| `NotificationSent` | After a successful delivery | Best-effort dispatch — listener errors don't propagate |
| `NotificationFailed` | When a channel returned an error | Best-effort dispatch; the underlying channel error still propagates per the first-failure-stops contract |

All three carry `(notification, channel, route, data)`. `Failed` adds
the stringified `error`. Listen with `EventFacade::listen::<E, L>` —
see [Events](events.md).

These events fire only on the synchronous `Notify::send` path. The
queued worker delivers channels directly without dispatching the
events.

### Telemetry

`NotificationDispatcher::notify` wraps the fan-out in a
`notification.dispatch` tracing span:

- `notification` — `Notification::notification_name()`
- `channel_count` — declared channel count
- `duration_ms` — fan-out latency on completion
- terminal log: `notification dispatched` (info) or
  `notification dispatch failed` (warn)

The mail channel nests its own `mail.send` span inside.

### First-failure-stops contract

`Notify::send` returns on the first channel error. Channels that
already succeeded are not rolled back; channels that haven't run yet
are not attempted. The same contract applies to the queued worker.

For at-least-once across multiple channels, dispatch each channel
through its own `Notify::queue` call — the queue envelope's
idempotency keys protect against double-sends on retry.

## Queued delivery

`Notify::send` runs in-process. `Notify::queue` pushes a
`SendNotificationJob` onto the [Queue](queues.md), pre-resolving the
per-channel routes from the recipient so the worker doesn't need a
`Notifiable` handle at execute time:

```rust
use suprnova::notifications::register_notification_factory;
use suprnova::Notify;

// At boot — once per concrete notification reachable via Notify::queue.
register_notification_factory::<OrderShipped>()?;

// Anywhere:
Notify::queue(&user, OrderShipped { tracking }).await?;
```

At dispatch time the worker:

1. Looks up the notification factory by `notification_name`
2. Reconstructs the typed notification from the JSON payload
3. Iterates the channels recorded at queue time
4. For each, looks up the channel on the bound dispatcher and calls
   `deliver(route, &notification)` directly

Channels that were declared at queue time but aren't registered when
the worker runs log a WARN and are skipped — same contract as the
synchronous path. Channels with no pre-resolved route are skipped
silently (the recipient returned `None` at queue time).

The queued path **does not** invoke `should_send`, `after_sending`, or
the three lifecycle events. If you depend on any of them, send through
`Notify::send` or move the logic inside the channel.

### Why Suprnova diverges

Laravel keys queued notifications off the `ShouldQueue` marker
interface — the same `Notification::send($user, $notification)` call
queues if the notification implements `ShouldQueue` and sends inline if
it doesn't. The behaviour depends on a type-level flag at the
notification site, which is invisible from the call site.

Suprnova makes that choice explicit at every call: `Notify::send` is
always synchronous; `Notify::queue` is always queued. There is no
hidden mode switch. (That's also why there's no `send_now` — `send` is
already the synchronous one.)

The recipient side diverges too. Laravel's `Notifiable` trait is a
mixin that pulls in the inbox relationship, `routeNotificationFor*`
methods, and the polymorphic primary key. Suprnova's `Notifiable` is
deliberately minimal — just `route_for(channel) -> Option<String>` —
because Rust traits don't compose by mixin. The Laravel-equivalent
read-side ships as free functions over `(notifiable_type,
notifiable_id)` (`unread_for`, `mark_as_read`, …) so plain structs
can be notifiable without inheriting an ORM relationship.

## Testing

Two fake surfaces, answering different questions.

### `Notify::fake()` — "was a notification dispatched?"

```rust
use suprnova::Notify;
use suprnova::notifications::{
    assert_count, assert_nothing_sent, assert_sent_named,
    assert_sent_times, assert_sent_to, assert_sent_to_on,
    recorded_notifications,
};

#[tokio::test]
async fn ship_dispatches_order_shipped() {
    let _fake = Notify::fake();

    Notify::send(
        &User { id: 1, email: "alice@example.org".into() },
        &OrderShipped { tracking: "1Z…".into() },
    ).await.unwrap();

    assert_sent_named("OrderShipped");
    assert_sent_to("alice@example.org", "OrderShipped");
    assert_sent_to_on("alice@example.org", "mail", "OrderShipped");
    assert_sent_times("OrderShipped", 1);
    assert_count(2); // mail + database
}
```

While the fake guard is alive, both `Notify::send` and `Notify::queue`
record the dispatch instead of running channels or enqueuing a job —
no channel runs, no queue row is written. The fake holds a
process-wide serialization mutex, so parallel tests cannot interleave
captures; let the `_fake` guard drop at end-of-test to clear the
recorder.

Use `recorded_notifications()` for full custody of the captured data:

```rust
let records = recorded_notifications();
assert_eq!(records[0].notification, "OrderShipped");
assert_eq!(records[0].channel, "mail");
assert_eq!(records[0].data["tracking"], "1Z…");
```

### `Mail::fake()` + real `MailChannel` — "did the notification *render* correctly?"

`Notify::fake()` short-circuits before the channel. To assert the mail
body actually rendered the way you expect, drive the real channel
under `Mail::fake()`:

```rust
use serial_test::serial;
use std::sync::Arc;
use suprnova::mail::Mail;
use suprnova::notifications::{set_dispatcher, NotificationDispatcher};
use suprnova::{MailChannel, Notify, register_mail_renderer};

#[tokio::test]
#[serial]
async fn ordershipped_renders_tracking_in_subject() {
    let fake = Mail::fake();
    register_mail_renderer::<OrderShipped>().unwrap();
    set_dispatcher(Arc::new(
        NotificationDispatcher::new()
            .register_channel(Arc::new(MailChannel::new())),
    )).unwrap();

    Notify::send(
        &User { id: 1, email: "alice@example.org".into() },
        &OrderShipped { tracking: "1Z…".into() },
    ).await.unwrap();

    fake.assert_sent_count(1);
    fake.assert_sent(|m| m.subject.contains("1Z…"));
}
```

Tests that touch the dispatcher, renderer, or transport globals must
be `#[serial_test::serial]` — those are process-global statics.

## Best practices

### Register every factory and renderer at boot

`Notify::queue` rebuilds the notification through the factory registry
at the worker, and `MailChannel` renders through `register_mail_renderer`.
Register every queueable / mailable notification up front:

```rust
// bootstrap.rs
use suprnova::notifications::register_notification_factory;
use suprnova::register_mail_renderer;

pub fn register() -> Result<(), FrameworkError> {
    // Notification factories (one per Notification reachable via Notify::queue).
    register_notification_factory::<OrderShipped>()?;
    register_notification_factory::<InvoicePaid>()?;

    // Mail renderers (one per NotificationMailable).
    register_mail_renderer::<OrderShipped>()?;
    register_mail_renderer::<InvoicePaid>()?;
    Ok(())
}
```

An unregistered notification on the queue surfaces as `unknown
notification: {name}` at worker execute time and retries through the
dead-letter path. A `MailChannel` dispatch for an unregistered renderer
surfaces a `register via suprnova::register_mail_renderer::<N>()` error
the same way.

### Queue for multi-channel fan-outs

The synchronous dispatcher visits channels in order and returns on the
first error. A failure on channel #2 leaves channel #1 committed and
channels #3+ unattempted. For any notification that crosses more than
one channel, prefer `Notify::queue` so the worker handles retries with
backoff and the dispatch survives a process crash.

### Make channel deliveries idempotent

Worker retries mean the same `SendNotificationJob` can execute more
than once. The built-in channels are idempotent-friendly: `MailChannel`
forwards to providers that typically dedupe by message-id;
`DatabaseChannel` inserts a fresh UUID per execution (which is the
right behaviour for an audit row); `WebPushChannel` POSTs to a
provider that swallows duplicates. Custom channels should target
idempotent operations — HTTP POSTs with stable client-side dedupe
keys, upserts rather than blind inserts, no "increment a counter"
side-effects on the delivery path.

### Bind the dispatcher in one place

`register_channel` is last-write-wins, so tests can swap a real
channel for a stub in setup. Keep the production binding in
`bootstrap.rs` and let tests build their own dispatcher with whatever
stubs they need. Don't `register_channel` lazily inside request
handlers — the global lock writes plus last-write-wins semantics get
surprising under concurrent load.

## Reference

| Symbol | Path |
|---|---|
| `Notifiable`, `Notification`, `Channel`, `DynNotification` | `suprnova::` |
| `Notify` (facade), `NotifyFakeGuard` | `suprnova::` |
| `NotificationDispatcher`, `NotificationFactory` | `suprnova::` |
| `AnonymousNotifiable` | `suprnova::` |
| `MailChannel`, `MailRendering`, `NotificationMailable` | `suprnova::` |
| `register_mail_renderer::<N>()` | `suprnova::` |
| `DatabaseChannel`, `StoredNotification` | `suprnova::` |
| `WebPushChannel` | `suprnova::` |
| `BroadcastChannel` | `suprnova::` |
| `SendNotificationJob` | `suprnova::` |
| `NotificationSending`, `NotificationSent`, `NotificationFailed` | `suprnova::` |
| `set_dispatcher`, `register_notification_factory` | `suprnova::notifications::` |
| `all_for`, `unread_for`, `read_for`, `mark_as_read`, `mark_as_unread`, `mark_all_as_read`, `delete_for` | `suprnova::notifications::` |
| `assert_sent`, `assert_sent_named`, `assert_sent_times`, `assert_sent_to`, `assert_sent_to_on`, `assert_nothing_sent`, `assert_nothing_sent_to`, `assert_count`, `recorded_notifications` | `suprnova::notifications::` |
| `#[derive(NotificationMailable)]` | `suprnova::` |

## Next

- [Mail](mail.md) — the transport and `Mailable` surface the mail channel rides on
- [Broadcasting](broadcasting.md) — the `BroadcastHub` the broadcast channel publishes through
- [Web Push](web-push.md) — VAPID, encryption, subscription storage
- [Events](events.md) — listening to `NotificationSending` / `Sent` / `Failed`
- [Queues](queues.md) — the worker that drives `Notify::queue`
- [Testing](testing.md) — fake surfaces and serial-test patterns
