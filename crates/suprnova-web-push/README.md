# suprnova-web-push

Web Push (RFC 8030 + RFC 8291 + RFC 8292) for the
[Suprnova](https://github.com/entrepeneur4lyf/suprnova) framework.

Ported from [`web-push 0.11.0`](https://crates.io/crates/web-push) with the
upstream `isahc`/`hyper 0.14` HTTP layer replaced by Suprnova's pinned
`reqwest 0.13`. The crypto (VAPID signing + ECE payload encryption) is
identical to upstream — only the transport changed.

## What you get

- `WebPushClient` — send encrypted notifications to FCM / Mozilla / Apple endpoints
- `VapidSigner` + `VapidKey` + `VapidClaims` — RFC 8292 application server signing
- `Payload` + `ContentEncoding` (aes128gcm) — RFC 8291 message encryption
- `SubscriptionInfo` — the browser-side subscription envelope
- `WebPushError` — typed error surface
- `EndpointPolicy` — denylist support for blocking subscriptions to specific
  push services (useful for compliance and abuse-control)

## Install

The framework re-exports the most common pieces under
`suprnova::notifications::web_push`, so application code rarely depends
on this crate directly. If you need the lower-level API:

```toml
[dependencies]
suprnova-web-push = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

## Usage

See `manual/web-push.md` in the Suprnova repo for the full guide, plus
`framework/src/notifications/web_push/` for higher-level integration with
the framework's `Notification` trait.

```rust,no_run
use suprnova_web_push::{WebPushClient, SubscriptionInfo, Payload, ContentEncoding, VapidSigner, VapidKey};

# async fn send() -> Result<(), Box<dyn std::error::Error>> {
let client = WebPushClient::new();
let vapid_key = VapidKey::from_pem(include_str!("../vapid_private.pem"))?;
let signer = VapidSigner::new(vapid_key, "mailto:admin@example.com");

let subscription: SubscriptionInfo = serde_json::from_str(/* from browser */ "{}")?;
let payload = Payload::new(b"hello", ContentEncoding::Aes128Gcm);

client.send(&subscription, &payload, &signer).await?;
# Ok(())
# }
```

## License

MIT
