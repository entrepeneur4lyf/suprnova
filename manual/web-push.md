# Web Push

Web Push delivers a short message to a browser even when your site is
closed — the Service Worker wakes up, decrypts the payload, and shows
an OS-level notification. Suprnova ships the protocol end-to-end:
VAPID key generation, AES128GCM payload encryption, the HTTP transport,
and a `WebPushChannel` that plugs into the notifications subsystem so
the same `Notification` you send to mail or database also lands as a
push.

Reach for this when you want to alert users in real time without an
open WebSocket — order shipped, friend request, mention, balance
posted. If the user is on a desktop browser with the site closed, web
push is the only mechanism that reaches them; if they're on the site,
[Broadcasting](broadcasting.md) is usually a better fit.

## The four pieces

Web Push has more moving parts than mail or database, because the
spec ([RFC 8030](https://datatracker.ietf.org/doc/html/rfc8030) +
[RFC 8291](https://datatracker.ietf.org/doc/html/rfc8291) +
[RFC 8292](https://datatracker.ietf.org/doc/html/rfc8292)) splits
identity, encryption, and transport across three contracts:

| Piece | What it is |
|---|---|
| `VapidKey` / `VapidSigner` | A P-256 ECDSA keypair used to sign JWTs that prove your server is who it claims to be |
| `WebPushClient` | The HTTP client that encrypts a payload, signs a VAPID JWT, and POSTs to the subscription's endpoint |
| `WebPushChannel` | The notifications-subsystem adapter that turns a `Notification` into a `WebPushClient::send` call |
| `SubscriptionInfo` | The opaque (`endpoint`, `p256dh`, `auth`) triple the browser hands you when a user subscribes — you store it; you don't generate it |

The bottom three layers — `VapidKey`, `WebPushClient`, the encrypted
POST — are re-exported from `suprnova::web_push` so applications never
need to depend on the underlying `suprnova-web-push` crate directly.

## Generate a VAPID keypair

Web Push uses VAPID (Voluntary Application Server Identification) to
let push services rate-limit and contact misbehaving senders. You need
one P-256 keypair per application; the public key goes into your
frontend so the browser can pin subscriptions to your server, and the
private key stays on the server signing JWTs.

Generate one once, persist it, and reuse it forever:

```rust
use suprnova::VapidKey;

let key = VapidKey::generate();

// Save the PEM somewhere durable — a secrets manager, a file the deploy
// pipeline mounts, an env-vars-as-files volume. You CANNOT regenerate
// this without invalidating every existing subscription.
let pem = key.to_pem()?;
std::fs::write("vapid_private.pem", &pem)?;

// The frontend needs the base64url-no-padding uncompressed public key.
// Hand this to your JS so `pushManager.subscribe()` can use it as
// `applicationServerKey`.
println!("PUBLIC_VAPID_KEY={}", key.public_key_uncompressed_b64url());
```

At boot, load the saved PEM:

```rust
use suprnova::{VapidKey, VapidSigner};

let pem = std::fs::read_to_string("vapid_private.pem")?;
let key = VapidKey::from_pem(&pem)?;
let signer = VapidSigner::new(key);
```

A `VapidSigner` produces JWTs but does not send anything — it's
purely a signing primitive. The next layer wraps it.

## Build a WebPushClient

`WebPushClient` is the HTTP-side primitive: feed it a signer and a
contact URI ("how the push service can reach you if you misbehave"),
get back an object whose `send` method encrypts a payload, signs a
JWT, and POSTs to the subscription endpoint.

```rust
use std::sync::Arc;
use suprnova::{VapidKey, VapidSigner, WebPushClient};

let signer = VapidSigner::new(VapidKey::from_pem(&pem)?);

// The subject MUST be a mailto: URI or an https: URL per RFC 8292 §2.1.
// Anything else is rejected at construction so a misconfigured deploy
// fails fast at boot — not silently after the first failed dispatch.
let client = WebPushClient::new(signer, "mailto:ops@example.org")?;

let client = Arc::new(client);
```

Why `Arc<WebPushClient>`? `WebPushClient` wraps a `VapidSigner` which
wraps a private `ES256KeyPair`. None of those are `Clone` — private
keys shouldn't be casually duplicated — and constructing a fresh
signer for every channel registration would mean N independent VAPID
identities for the same application. Wrapping in `Arc` lets a single
signed identity back every registration and every concurrent delivery.

### Endpoint policy

Subscription endpoints are user-derived data: the browser receives the
URL from a remote push service when a user subscribes, and your server
stores whatever the browser handed back. A maliciously stored
subscription can point the HTTP POST anywhere reachable, turning the
push sender into an SSRF gadget.

`WebPushClient` defaults to `EndpointPolicy::Strict`:

- Scheme must be `https`
- Host must be a named domain, not an IP literal
- Cloud-metadata hostnames and RFC 2606 reserved TLDs (`.localhost`,
  `.local`, `.internal`, `.test`, `.example`, `.invalid`) are rejected

This blocks the obvious SSRF probes without breaking real push
services (FCM, Mozilla Autopush, Apple's `web.push.apple.com`).

For local integration tests against a `wiremock` mock server you have
to opt out:

```rust
use suprnova::{EndpointPolicy, WebPushClient};

let client = WebPushClient::new(signer, "mailto:test@example.org")?
    .with_endpoint_policy(EndpointPolicy::AllowAny);
```

Do not use `AllowAny` in production. The strict checks exist to keep
a tampered subscriptions table from being weaponised.

### Custom transport

`WebPushClient::new` applies a 30-second per-request timeout. If you
need a different transport policy — corporate proxy, pinned TLS,
shorter timeout — build a `reqwest::Client` and use
`WebPushClient::with_client`:

```rust
use reqwest::Client;
use std::time::Duration;
use suprnova::WebPushClient;

let http = Client::builder()
    .timeout(Duration::from_secs(10))
    .build()?;

let client = WebPushClient::with_client(http, signer, "mailto:ops@example.org")?;
```

## Wire WebPushChannel into notifications

The raw `WebPushClient::send` works — but the way you actually send
push notifications in Suprnova is through the
[Notifications](notifications.md) subsystem. A `Notification` declares
`vec!["webpush"]` in its `channels()`, a `Notifiable` recipient
returns a JSON-encoded `SubscriptionInfo` from `route_for("webpush")`,
and the bound `NotificationDispatcher` does the fan-out.

```rust
use std::sync::Arc;
use suprnova::{
    NotificationDispatcher, WebPushChannel, WebPushClient,
    notifications::set_dispatcher,
};

let client: Arc<WebPushClient> = Arc::new(
    WebPushClient::new(signer, "mailto:ops@example.org")?
);

// ttl_secs: how long the push service holds an undelivered message.
// 86_400 (24h) is a reasonable default for non-urgent notifications;
// drop to 60 for "act right now" alerts where a stale message is
// worse than no message.
let webpush = Arc::new(WebPushChannel::new(client, 86_400));

let dispatcher = NotificationDispatcher::new()
    .register_channel(webpush);

set_dispatcher(Arc::new(dispatcher))?;
```

`register_channel` is last-write-wins on the channel's `name()`, so
tests can swap in a stub without affecting the production binding.

## Define a notification

A push-bound notification is the same shape as any other Suprnova
notification — declare `"webpush"` in `channels()` and put whatever
JSON you want delivered into `data()`:

```rust
use serde::{Deserialize, Serialize};
use suprnova::Notification;

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct OrderShipped {
    pub order_id: i64,
    pub tracking_url: String,
}

impl Notification for OrderShipped {
    fn notification_name() -> &'static str {
        "OrderShipped"
    }

    fn channels(&self) -> Vec<&'static str> {
        vec!["webpush"]
    }

    fn data(&self) -> serde_json::Value {
        serde_json::json!({
            "title":   "Your order has shipped",
            "body":    format!("Track order #{}", self.order_id),
            "url":     self.tracking_url,
        })
    }
}
```

The `data()` JSON is what your Service Worker receives. Pick a stable
shape and document it for the frontend — Suprnova doesn't impose one,
because notification UI is a frontend concern.

## Route the recipient

A `Notifiable` returns the route for each channel it supports.
For Web Push, that route is the JSON-encoded `SubscriptionInfo` —
exactly what the browser produced via `PushSubscription.toJSON()`,
stored verbatim:

```rust
use suprnova::Notifiable;

pub struct User {
    pub id: i64,
    pub push_subscription_json: Option<String>,
}

impl Notifiable for User {
    fn route_for(&self, channel: &str) -> Option<String> {
        match channel {
            "webpush" => self.push_subscription_json.clone(),
            _ => None,
        }
    }
}
```

Returning `None` causes the dispatcher to skip the channel silently —
useful for users who haven't subscribed to push but still get email.

## Send it

Synchronous:

```rust
use suprnova::Notify;

let user = User::find(42).await?.unwrap();
Notify::send(&user, &OrderShipped {
    order_id: 1234,
    tracking_url: "https://ship.example.org/o/1234".into(),
}).await?;
```

Queued — pre-resolves the subscription route at queue time so the
worker doesn't need to re-load the user:

```rust
Notify::queue(&user, OrderShipped {
    order_id: 1234,
    tracking_url: "https://ship.example.org/o/1234".into(),
}).await?;
```

For `Notify::queue` to work, register the notification's factory at
boot so the worker can rebuild the JSON payload into the typed
notification:

```rust
suprnova::notifications::register_notification_factory::<OrderShipped>()?;
suprnova::queue::register_job::<suprnova::SendNotificationJob>();
```

Behind the scenes, queued dispatch builds a `SendNotificationJob`
carrying `(notification_name, payload, per_channel_routes, channels)`.
The worker re-hydrates the notification, looks up `WebPushChannel` by
name on the bound dispatcher, and calls `deliver(route, &notification)`
— the same code path as the synchronous `Notify::send`.

## The browser side

Suprnova does not ship a JavaScript SDK — the browser side is plain
Web Push API. The flow your frontend needs to implement:

1. Register a Service Worker.
2. Ask the user for permission.
3. Subscribe via `pushManager.subscribe({ userVisibleOnly: true,
   applicationServerKey: <your VAPID public key> })`.
4. POST `subscription.toJSON()` to a Suprnova endpoint that stores
   it on the user row.

```js
// Service Worker registration (somewhere in your app entrypoint)
const registration = await navigator.serviceWorker.register('/sw.js');

if (Notification.permission === 'default') {
    await Notification.requestPermission();
}

if (Notification.permission === 'granted') {
    const subscription = await registration.pushManager.subscribe({
        userVisibleOnly: true,
        applicationServerKey: window.PUBLIC_VAPID_KEY,
    });

    await fetch('/api/push/subscribe', {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(subscription.toJSON()),
    });
}
```

Your Suprnova endpoint receives the JSON, validates the shape, and
stores it on the user — the string is opaque to your server, but it
must be the exact JSON the browser produced (the
`SubscriptionInfo` type uses `Deserialize` to parse it later):

```rust
use suprnova::{Auth, Request, Response, SubscriptionInfo, attrs, json_response};

pub async fn subscribe(req: Request) -> Response {
    let user_id = Auth::id().expect("auth middleware");

    let (_parts, bytes) = match req.body_bytes().await {
        Ok(b) => b,
        Err(e) => return json_response!({ "error": e.to_string() }, 400),
    };
    let raw = match std::str::from_utf8(&bytes) {
        Ok(s) => s.to_string(),
        Err(_) => return json_response!({ "error": "body not utf-8" }, 400),
    };

    // Parse to validate the shape — endpoint, keys.p256dh, keys.auth.
    // If parsing fails, the browser handed us something malformed.
    let sub: SubscriptionInfo = match serde_json::from_str(&raw) {
        Ok(s) => s,
        Err(e) => return json_response!({ "error": e.to_string() }, 400),
    };

    // Persist `raw` verbatim — that's the exact string WebPushChannel
    // will hand to serde_json::from_str on dispatch.
    User::query()
        .db_where("id", "=", user_id)
        .update(attrs! { push_subscription_json: raw })
        .await
        .unwrap();

    json_response!({ "ok": true, "endpoint": sub.endpoint })
}
```

The Service Worker decrypts the push payload and renders the
notification:

```js
// /sw.js
self.addEventListener('push', (event) => {
    const data = event.data.json();
    event.waitUntil(
        self.registration.showNotification(data.title, {
            body: data.body,
            data: { url: data.url },
        }),
    );
});

self.addEventListener('notificationclick', (event) => {
    event.notification.close();
    event.waitUntil(clients.openWindow(event.notification.data.url));
});
```

## Payload limits

The Web Push spec caps each encrypted payload at 4096 bytes total.
Suprnova rejects plaintexts larger than 3992 bytes (the cap minus the
~85-byte AES128GCM encryption overhead) at encrypt time so the
failure surfaces in your code, not in a 413 from the push service.
A `Notification` whose serialized `data()` exceeds that limit
returns `WebPushError::Encryption` from the channel's `deliver`.

For anything larger — a long message body, a thumbnail — send a short
notification carrying a URL the Service Worker fetches on click. That's
both faster (no encryption on a multi-KB payload) and more flexible
(the fetch can return whatever shape you want).

## Dead subscriptions

When the push service returns 404 or 410, the subscription is dead —
the user uninstalled the browser, revoked the permission, or cleared
storage. `WebPushChannel` treats this as a non-fatal warn:

```text
WARN webpush subscription gone (404/410); caller should remove
     channel=webpush endpoint=https://fcm.googleapis.com/fcm/send/abc
```

Dispatch returns `Ok(())` because the notification reached a terminal
state — there's no recipient to retry against. Your application is
expected to act on the warn: parse `endpoint` from the log (or hook a
`NotificationFailed` listener that classifies via `WebPushError`) and
remove the subscription row. Suprnova ships the warn; it does not
auto-prune the subscriptions table for you.

## Retries and Retry-After

When the push service returns a transient 5xx, 408, or 429, the
underlying `WebPushError::PushServiceRejected` carries the parsed
`Retry-After` hint (delta-seconds form only — HTTP-date form returns
`None`):

```rust
use suprnova::WebPushError;

match client.send(&sub, payload, ContentEncoding::Aes128Gcm, 60).await {
    Ok(_) => (),
    Err(e) if e.is_retryable() => {
        let wait = e.retry_after().unwrap_or(Duration::from_secs(30));
        tokio::time::sleep(wait).await;
        // ...try again, or push back onto the queue with a delay
    }
    Err(WebPushError::SubscriptionGone) => {
        // remove the subscription
    }
    Err(e) => return Err(e.into()),
}
```

The `Retry-After` hint is capped at 24 hours so a hostile server
can't park a worker on a multi-year sleep.

When using `Notify::queue`, the queue's own retry/backoff applies —
a `WebPushError` that propagates out of `WebPushChannel::deliver`
surfaces as a job error and the envelope handles re-queueing per the
job's backoff policy. The `Retry-After` hint is logged but not (yet)
fed back into the queue's delay computation; if you need that, hook
a `NotificationFailed` listener that re-queues with the hinted delay.

## Telemetry

The notifications dispatcher wraps the fan-out in a
`notification.dispatch` info span tagged with the notification name
and channel count. Each successful delivery emits a
`NotificationSent` event; failures emit `NotificationFailed` carrying
the channel name, route, and error string. Wire any of those into
your metrics/log pipeline the same way you wire other framework
events — see [Events](events.md).

A dead subscription emits a structured WARN with `channel="webpush"`,
the endpoint, and the notification name. That's the signal to scrape
for an automated subscription cleanup job.

### Why Suprnova diverges

Laravel's `WebPush` driver is a community package
(`laravel-notification-channels/webpush`) — not in core, separately
versioned, opinionated about ORM. Suprnova bakes Web Push into the
framework because the protocol is well-defined and the encrypted
HTTP POST is too small a contract to wrap in a third-party
abstraction. The notifications subsystem keeps the surface uniform:
the same `Notification` you send to mail or database also lands as a
push, no driver matrix, no separate config tree.

We also surface the strict-endpoint policy by default. The Laravel
community package leaves SSRF protection to the application; we
take the position that "the endpoint came from user data" is the
shape of every Web Push subscription, and the safe default belongs
in the framework, not in your code.

The retry classification (`is_retryable`, `retry_after`) is exposed
as typed methods on `WebPushError` rather than as a magic constant
table in the queue layer. The queue still owns retry policy — the
error tells you whether a retry could succeed and how long to wait;
the queue decides whether and when to dequeue again. Separating the
two means your custom retry strategies (exponential backoff,
jittered, capped) don't have to special-case Web Push.

## Testing

Stand up a `wiremock` server, point a `WebPushClient` at it with
`EndpointPolicy::AllowAny`, and assert on the requests it receives:

```rust
use std::sync::Arc;
use suprnova::{
    EndpointPolicy, NotificationDispatcher, Notify, VapidKey, VapidSigner,
    WebPushChannel, WebPushClient,
    notifications::set_dispatcher,
};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn order_shipped_pushes() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/push"))
        .respond_with(ResponseTemplate::new(201))
        .mount(&server)
        .await;

    let signer = VapidSigner::new(VapidKey::generate());
    let client = Arc::new(
        WebPushClient::new(signer, "mailto:test@example.org")
            .unwrap()
            .with_endpoint_policy(EndpointPolicy::AllowAny),
    );
    let channel = Arc::new(WebPushChannel::new(client, 60));

    let dispatcher = NotificationDispatcher::new().register_channel(channel);
    set_dispatcher(Arc::new(dispatcher)).unwrap();

    let user = test_user_with_subscription(&server.uri()).await;
    Notify::send(&user, &OrderShipped {
        order_id: 1,
        tracking_url: "https://ship.example.org/o/1".into(),
    }).await.unwrap();
    // server.received_requests() now contains the encrypted POST.
}
```

For end-to-end tests that don't care about the encrypted bytes,
`Notify::fake()` (covered in [Notifications](notifications.md))
captures the dispatch without running the channel — faster, no
mock server, no encryption round-trip.

## Reference

- Primitives: `suprnova::VapidKey`, `suprnova::VapidSigner`,
  `suprnova::VapidClaims`
- Client: `suprnova::WebPushClient`, `suprnova::EndpointPolicy`,
  `suprnova::PushResponse`, `suprnova::SubscriptionInfo`
- Error: `suprnova::WebPushError` — `.is_retryable()`, `.retry_after()`,
  `WebPushError::SubscriptionGone`
- Encoding: `suprnova::ContentEncoding` (Aes128Gcm; 3992-byte plaintext cap)
- Channel: `suprnova::WebPushChannel`
- Facade: `suprnova::Notify`
- Queue job: `suprnova::SendNotificationJob`
- Factory registration:
  `suprnova::notifications::register_notification_factory`

## Next

- [Notifications](notifications.md) — the multi-channel dispatcher that
  `WebPushChannel` plugs into
- [Mail](mail.md) — the email-channel counterpart for users without push
- [Broadcasting](broadcasting.md) — real-time delivery for users who are
  on the site
- [Queues](queues.md) — how `Notify::queue` backs `SendNotificationJob`
- [Events](events.md) — listening for `NotificationSent` /
  `NotificationFailed` to drive dead-subscription cleanup
