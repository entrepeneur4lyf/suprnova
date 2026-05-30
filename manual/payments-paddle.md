# Payments — Paddle Adapter

The Paddle adapter (`suprnova-payments-paddle`) wires Paddle into Suprnova's
generic payments surface. Reach for it when you want a payment provider that
also handles sales tax, VAT, GST, dunning, invoicing, and refunds on your
behalf — Paddle is a Merchant of Record (MoR), which means it is the seller
of record to your customers and absorbs the compliance surface that a
direct-capture gateway like Stripe leaves to you.

That choice changes the mental model. Your domain code does not *own* the
subscription — Paddle does. You open a checkout, the customer completes it,
and the `SubscriptionCreated` webhook tells you the subscription now exists.
You cannot create a subscription via API, and you cannot swap its price set
after the fact. You can cancel, you can read state, you can update billing
metadata. The rest is Paddle's.

This chapter assumes you've read [Payments](payments.md) for the generic
five-trait surface. Here we cover what is true *only* for Paddle.

## When to pick Paddle

Pick Paddle when one or more is true:

- You sell digital products globally and tax compliance (VAT, GST, US sales
  tax) is a real cost on your roadmap.
- You don't want to manage failed-payment retries, dunning emails, or
  receipt-issuing yourself.
- You want a single invoice from a single seller-of-record for accounting.
- Your business model is subscription-first, and you accept that the
  provider drives the subscription lifecycle.

Pick [Stripe](payments.md#stripe) instead when you want direct control over
charge capture, you handle your own tax, or you need server-side
`charge`/`capture`/`refund` calls from your own code paths.

## Setup

Add the crate:

```bash
cargo add suprnova-payments-paddle
```

Set the four environment variables:

```env
PADDLE_API_KEY=pdl_sdbx_apikey_...
PADDLE_WEBHOOK_KEY=pdl_ntfset_...
PADDLE_CLIENT_TOKEN=test_...
PADDLE_ENVIRONMENT=sandbox
```

| Variable | What it is | Where it comes from |
|---|---|---|
| `PADDLE_API_KEY` | Server-side API key (`pdl_live_apikey_…` / `pdl_sdbx_apikey_…`) | Paddle dashboard → Developer Tools → Authentication |
| `PADDLE_WEBHOOK_KEY` | Notification destination secret (`pdl_ntfset_…`) | Paddle dashboard → Developer Tools → Notifications → your endpoint |
| `PADDLE_CLIENT_TOKEN` | Browser-safe client token (`live_…` / `test_…`) | Paddle dashboard → Developer Tools → Authentication → Client-side tokens |
| `PADDLE_ENVIRONMENT` | `sandbox` (default) or `production` | Your call |

Register the provider at bootstrap. Both forms are valid:

```rust
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;
use suprnova_payments_paddle::{PaddleEnvironment, PaddleProvider};

pub async fn bootstrap() {
    // From env (recommended):
    let paddle = PaddleProvider::from_env()
        .expect("Paddle env vars not set");

    // Or construct directly:
    let paddle = PaddleProvider::new(
        "pdl_sdbx_apikey_...",
        "pdl_ntfset_...",
        "test_...",
        PaddleEnvironment::Sandbox,
    ).expect("Paddle client init failed");

    PaymentProviderRegistry::bind("paddle", Arc::new(paddle));
}
```

The webhook ingress route is registered by the framework's
`webhook_routes(db.clone())` helper — see [Payments](payments.md#webhook-handling).
Both `from_env()` and `new()` return `Result` because the underlying
`paddle_rust_sdk::Paddle::new` validates the API key shape and the
endpoint URL at construction time.

## The MoR mental model

The shape that surprises Stripe users:

```
Stripe (gateway):
    your app  ─────────►  Stripe  ──►  card network
       │                    ▲
       └────── webhook ─────┘
    you own the subscription state in your DB; Stripe is the executor

Paddle (Merchant of Record):
    your app  ─►  checkout link  ─►  customer  ──►  Paddle  ──►  card network
                                                       │
       ◄──────────────────  webhook  ──────────────────┘
    Paddle owns the subscription state; your DB is the mirror
```

In code, the difference shows up at three points:

1. **You cannot create a subscription via API.** Call `Checkout::start_session`
   with a recurring price; the customer completes the Paddle widget; the
   `SubscriptionCreated` webhook hydrates your mirror.
2. **You cannot swap a subscription's price set via API.** Paddle reserves
   plan changes for its own dashboard or for migration flows it owns.
3. **You cannot delete a customer.** Archive via update is the supported
   workaround.

Suprnova surfaces these constraints as `PaymentError::NotSupported` rather
than papering over them — see the [capability matrix](#capability-matrix)
below.

## Checkout flow

`Checkout::start_session` is the only way to start a payment with Paddle.
The frontend opens the resulting `transaction_id` with paddle.js using the
`client_token` you set at bootstrap:

```rust
use std::sync::Arc;
use suprnova::payments::*;

pub async fn start_checkout(
    user_id: String,
    email: String,
) -> PaymentResult<SessionPayload> {
    let provider = PaymentProviderRegistry::get("paddle")
        .expect("paddle provider not registered");

    // 1. Create the customer in Paddle (or reuse an existing one).
    let cus = provider.create_customer(CreateCustomerRequest {
        user_id: user_id.clone(),
        email,
        name: None,
        metadata: None,
    }).await?;

    // 2. Open a checkout session. Paddle dispatches one-off vs subscription
    //    on the *price kind*, not on the SessionMode field below.
    let session = provider.start_session(StartSessionRequest {
        mode: SessionMode::Subscription,           // ignored by Paddle (see note)
        customer_ref: cus.provider_customer_id,
        price_refs: vec!["pri_pro_monthly".into()],
        success_return_url: "https://app.example/billing/success".into(),
        cancel_return_url: "https://app.example/billing/cancel".into(),
        amount_hint: None,
        idempotency_key: Some(format!("checkout_{user_id}")),
        metadata: None,
    }).await?;

    Ok(session)
}
```

The returned `SessionPayload::PaddleInline` carries everything the frontend
needs:

```json
{
  "flow": "paddle_inline",
  "transaction_id": "txn_01h...",
  "customer_token": "ctm_01h...",
  "client_token": "test_..."
}
```

See [Payments — Frontend Integration](payments-frontend.md) for the
paddle.js mounting code in Svelte / React / Vue.

### Paddle dispatches on price kind, not `SessionMode`

A genuine Paddle-specific gotcha: the `SessionMode::OneOff` /
`SessionMode::Subscription` field on `StartSessionRequest` is **ignored by
the Paddle adapter**. Paddle's API has a single `transaction_create`
endpoint, and the provider inspects the supplied price IDs to infer the
flow — a recurring price starts a subscription, a one-off price starts a
single charge. With Stripe the field drives the flow; with Paddle the
*price* does. Set up your Paddle catalog with the correct price kinds
before pointing the adapter at them.

## Subscriptions arrive via webhook

Because Paddle owns the subscription lifecycle, your domain code only
*learns* about a subscription when Paddle tells you. The flow:

```
your app                        Paddle                    customer
   │                              │                          │
   │  start_session(price=pri_…)  │                          │
   ├─────────────────────────────►│                          │
   │  PaddleInline { txn_id, … }  │                          │
   │◄─────────────────────────────┤                          │
   │                              │       paddle.js          │
   │                              │◄─────────────────────────┤
   │                              │   complete checkout      │
   │                              ├─────────────────────────►│
   │                              │                          │
   │   subscription.created webhook                          │
   │◄─────────────────────────────┤                          │
   │                              │                          │
   ▼                              │                          │
 mirror tables hydrated;          │                          │
 payments_subscriptions row       │                          │
 has provider_subscription_id     │                          │
```

The framework's `webhook_routes(db)` handler does the hydration for you:
it calls `WebhookHandler::extract_payload_ids` to find the
`subscription_id`, calls `Subscription::get(id)` to read the canonical
state, and upserts `payments_subscriptions` + `payments_subscription_items`
inside one transaction. By the time the webhook returns 200, your mirror
is consistent with Paddle.

There is a brief window between the customer completing the widget and
the webhook arriving in which `payments_subscriptions` has no row for
the new subscription. Two patterns cover it:

- **Use the redirect URL for immediate UX.** `success_return_url` fires
  client-side as soon as Paddle confirms the transaction, so you can show
  "Subscription active" without waiting for the server-side webhook.
- **Poll-and-render.** After the redirect, refresh the page after a short
  delay so the Inertia controller can read the now-hydrated mirror.

## Capability matrix

Not every method on every trait does what its Stripe equivalent does. The
table below is the truth. `subscribe()` and `update()` with
`new_price_refs.is_some()` are the only methods that *always* fail; the
rest work, with the noted caveats.

| Trait method | Behavior |
|---|---|
| `Checkout::start_session` | Works. Dispatches one-off vs subscription on price kind, not `SessionMode`. |
| `Subscription::subscribe` | Always `NotSupported`. Subscriptions are born from checkout completion + webhook. |
| `Subscription::update(cancel_at_period_end: Some(true), new_price_refs: None)` | Works. Wires to `subscription_cancel` with default `EffectiveFrom::NextBillingPeriod`. |
| `Subscription::update(new_price_refs: Some(...))` | `NotSupported` in v1. Paddle reserves price-set replacement for its own migration flows. |
| `Subscription::update` (no-op) | Works. Re-fetches current state via `subscription_get`. |
| `Subscription::cancel` | Works, but `at_period_end` is **ignored** — always schedules to next billing period. See [below](#cancellation-is-always-scheduled). |
| `Subscription::get` | Works. |
| `CustomerStore::create_customer` | Works. |
| `CustomerStore::update_customer` | Works. |
| `CustomerStore::get_customer` | Works. |
| `CustomerStore::delete_customer` | `NotSupported`. Use `update_customer` with `archived` status if needed. |
| `Payment::*` | Trait is not implemented. `provider.as_payment()` returns `None`. |
| `WebhookHandler::*` | Works. |

The invariants `Payment` not being implemented, `subscribe`/`delete_customer`
returning `NotSupported`, and webhook signature rejection are pinned by
always-on tests in `crates/suprnova-payments-paddle/tests/integration.rs`,
so the matrix above won't drift silently.

### Cancellation is always scheduled

`Subscription::cancel(id, at_period_end)` accepts the bool for trait
compatibility but **always behaves as scheduled cancellation** —
Paddle's `EffectiveFrom` enum is private in `paddle_rust_sdk` 0.18, so
immediate cancel is not viable in v1. The user keeps access until the
current billing period ends, at which point Paddle fires
`subscription.canceled` and the mirror flips `status` to `Canceled`.

If you want a UX-level "cancel now" that revokes app access immediately
while letting Paddle wind down billing in the background, gate access on
your own `subscription.status != Canceled && subscription.cancel_at_period_end == false`
flag and update the UI right after `cancel()` returns — the next webhook
will confirm.

### Customer deletion is "archive via update"

`delete_customer` returns `PaymentError::NotSupported` because Paddle's
public API does not expose a delete endpoint at all. If you need to
suppress a customer record in Paddle, call `update_customer` with the
`archived` status. The framework adapter does not wrap this directly —
the metadata field is the escape hatch:

```rust
provider.update_customer(UpdateCustomerRequest {
    provider_customer_id: customer_id,
    email: None,
    name: None,
    metadata: Some(serde_json::json!({ "status": "archived" })),
}).await?;
```

Confirm the exact field path against your Paddle API version when shipping
this — the SDK does not currently model the `status` enum directly.

## Webhook signature verification

Paddle signs every webhook with HMAC. The `Paddle-Signature` header looks
like `ts=1716000000,h1=abcdef…`. The adapter delegates verification to
`Paddle::unmarshal` from the SDK, which:

- Parses the header
- Recomputes the HMAC using your `PADDLE_WEBHOOK_KEY`
- Rejects signatures whose timestamp is outside `MaximumVariance::default()`
  (5 seconds at time of writing — replays older than that are dropped)

The framework's `webhook_routes` handler calls `verify` before doing
anything else; a failure returns `401 invalid-signature` with no body
leak. You don't write any of this code yourself, but it's worth knowing
the verification is HMAC + timestamp-tolerance, not a static secret
compare.

## Webhook payload shape

The adapter's `extract_payload_ids`, `extract_payment_snapshot`, and
`extract_customer_snapshot` methods know Paddle's payload shape so the
framework can hydrate mirror tables. Quick mapping:

| Webhook event_type | `NeutralEventKind` | Mirror effect |
|---|---|---|
| `transaction.completed`, `transaction.paid` | `PaymentSucceeded` | Upsert `payments_transactions` |
| `transaction.payment_failed` | `PaymentFailed` | Upsert `payments_transactions` (failed) |
| `transaction.billed` | `InvoicePaid` | Upsert `payments_transactions` with `provider_subscription_id` linked |
| `adjustment.created`, `adjustment.updated` | `PaymentRefunded` | Upsert `payments_transactions` (refunded) |
| `subscription.created` | `SubscriptionCreated` | `Subscription::get` → upsert `payments_subscriptions` + items |
| `subscription.updated`, `.activated`, `.paused`, `.resumed`, `.trialing` | `SubscriptionUpdated` | Same as above |
| `subscription.canceled` | `SubscriptionCanceled` | Same; sets `canceled_at`, flips status |
| `customer.created` | `CustomerCreated` | Update-only: refreshes `email`/`metadata` if the mirror row exists |
| `customer.updated` | `CustomerUpdated` | Same |
| anything else | `None` (unmapped) | Audit row only — no mirror change |

Paddle puts the entity object directly under `data` (not `data.object` like
Stripe). Amounts arrive as **strings of minor units** (`"1234"` = 12.34 in
the major unit), not decimals — the adapter parses both string and
numeric shapes for forward-compatibility. Currency arrives as
`currency_code`, lower-case, and the snapshot upper-cases it.

### Inclusive-tax amounts

Paddle reports transaction amounts **inclusive of tax**. The framework's
`payments_transactions` mirror splits this:

- `amount_total_minor` — the full amount the customer paid (tax included)
- `amount_tax_minor` — the tax component

Net of tax is `amount_total_minor - amount_tax_minor`. This differs from
Stripe (which reports exclusive of tax with `amount_tax_minor = 0`). Code
that sums revenue across both providers needs to be tax-aware:

```rust
let net_revenue_minor = txn.amount_total_minor - txn.amount_tax_minor;
```

## Customer creation

`CreateCustomerRequest` maps directly to Paddle's `customer_create`:

```rust
let cus = provider.create_customer(CreateCustomerRequest {
    user_id: "user_42".into(),       // your app's user id
    email: "alice@example.com".into(),
    name: Some("Alice".into()),
    metadata: None,                  // not forwarded to Paddle in v1
}).await?;
// cus.provider_customer_id == "ctm_01h..."
```

Store `cus.provider_customer_id` alongside your user record. Every
subsequent call (start a checkout, look up a subscription, etc.) takes
the Paddle customer ID, not the app's user ID. The mirror table
`payments_customers` carries both columns so a single index lookup gets
you either direction.

`update_customer` and `get_customer` pass through to the equivalent SDK
methods. `update_customer` accepts `email` / `name` updates and returns
the refreshed `CustomerRef`. `get_customer` fetches a snapshot from
Paddle (not from the mirror) — use this when you need a fresh read after
an out-of-band change in the Paddle dashboard.

## The intentional `NotSupported` shape

A reader unfamiliar with the codebase might assume `PaymentError::NotSupported`
on `subscribe()` and `delete_customer()` is a deferred TODO. It is not.
The constraints are part of Paddle's product surface, and Suprnova
encodes them rather than emulating local mutations the provider will
never honor.

Each `NotSupported` error message points at the supported workflow:

- `subscribe`: "use `Checkout::start_session` with `SessionMode::Subscription`
  and await the `SubscriptionCreated` webhook"
- `update` with `new_price_refs`: "Paddle price-set replacement on existing
  subscription not in v1"
- `delete_customer`: "use `UpdateCustomer` with `archived` status"

Branch on this error explicitly when you're writing provider-agnostic
domain code:

```rust
match provider.delete_customer(&cus_id).await {
    Ok(()) => { /* Stripe path */ }
    Err(PaymentError::NotSupported(_)) => {
        // Paddle path — archive via update instead
        provider.update_customer(UpdateCustomerRequest {
            provider_customer_id: cus_id,
            email: None,
            name: None,
            metadata: Some(serde_json::json!({ "status": "archived" })),
        }).await?;
    }
    Err(e) => return Err(e),
}
```

### Why Suprnova diverges

Laravel Cashier is Stripe-only and models subscriptions as
app-owned: `$user->newSubscription('default', 'pri_pro')->create()` is
shaped as if the application is initiating the subscription. With a
direct-capture gateway that's accurate. With an MoR, it's a lie — the
provider is the actor, not your app.

Suprnova's payments surface is provider-neutral, so it doesn't take a
side. The trait surface (`subscribe`, `update`, `cancel`, `get`) is the
generic shape; each adapter implements what its provider exposes and
returns `NotSupported` where the provider's product model differs. The
Stripe adapter implements `subscribe`. The Paddle adapter does not,
because Paddle does not let it. Hiding the difference behind a fake
local "create" would have the adapter lie to you — Suprnova prefers
the typed `NotSupported` with a migration message in the error string.

The same divergence applies to `Payment` (server-side capture). Stripe
implements it; Paddle does not, and `provider.as_payment()` returns
`None`. Code that needs charge/capture/refund must check
`as_payment().is_some()` rather than calling blindly — see
[Payments](payments.md#payment--optional-server-side-capture).

## Testing your integration

The crate includes always-on invariant tests (no network access needed)
plus an env-gated integration test against Paddle's sandbox API:

```bash
# Always-on invariants (signature rejection, NotSupported shapes):
cargo test -p suprnova-payments-paddle

# Plus sandbox integration (requires PADDLE_API_KEY etc.):
PADDLE_API_KEY=pdl_sdbx_apikey_... \
PADDLE_WEBHOOK_KEY=pdl_ntfset_... \
PADDLE_CLIENT_TOKEN=test_... \
PADDLE_ENVIRONMENT=sandbox \
  cargo test -p suprnova-payments-paddle
```

The invariant tests are the ones to mirror in your own code if you build
adapter-specific abstractions. Three test shapes worth copying:

```rust
use suprnova::payments::*;
use suprnova_payments_paddle::{PaddleEnvironment, PaddleProvider};

#[test]
fn paddle_does_not_implement_payment_trait() {
    let p = PaddleProvider::new(
        "pdl_sdbx_apikey_test",
        "pdl_ntfset_test",
        "test_client",
        PaddleEnvironment::Sandbox,
    ).expect("provider construction");
    assert!(p.as_payment().is_none());
}

#[tokio::test]
async fn paddle_subscribe_returns_not_supported() {
    let p = /* ...as above... */;
    let err = p.subscribe(SubscribeRequest {
        customer_ref: "ctm_test".into(),
        price_refs: vec!["pri_test".into()],
        trial_days: None,
        idempotency_key: None,
        metadata: None,
    }).await.unwrap_err();
    assert!(matches!(err, PaymentError::NotSupported(_)));
}

#[test]
fn webhook_verify_rejects_bad_signature() {
    let p = /* ...as above... */;
    let mut headers = http::HeaderMap::new();
    headers.insert("paddle-signature", "ts=1234,h1=deadbeef".parse().unwrap());
    let ctx = WebhookContext {
        body: b"{}",
        headers: &headers,
        remote_addr: None,
    };
    assert!(matches!(p.verify(&ctx).unwrap_err(), PaymentError::WebhookSignature(_)));
}
```

For local end-to-end testing without hitting Paddle at all, the framework
ships `MockPaymentProvider`. Like Paddle, the mock's `as_payment()`
returns `None` (no server-side capture), so code that branches on
`as_payment().is_some()` follows the same path under the mock as it will
under Paddle. The mock's `subscribe()` returns `Ok` (unlike Paddle), so
tests that need to assert the `NotSupported` branch should use the real
`PaddleProvider`. Bind the mock in tests instead of the real provider:

```rust
use std::sync::Arc;
use suprnova::payments::{MockPaymentProvider, PaymentProviderRegistry};

#[suprnova_test]
async fn checkout_flow() {
    PaymentProviderRegistry::bind("paddle", Arc::new(MockPaymentProvider::new()));
    // ...exercise your controller against the mock...
}
```

## Production checklist

Before flipping `PADDLE_ENVIRONMENT=production`:

- [ ] All four env vars are set in production secrets, not committed
- [ ] The webhook endpoint URL is registered in the Paddle dashboard
  *Notifications* settings, and the destination secret you generated
  there matches `PADDLE_WEBHOOK_KEY`
- [ ] The catalog has live (not sandbox) price IDs, and the IDs you
  reference in `price_refs` exist in the live catalog
- [ ] Your `success_return_url` and `cancel_return_url` point at HTTPS
  endpoints (Paddle rejects HTTP in production)
- [ ] You've decided how your app responds when `subscribe()`,
  `delete_customer()`, or `update(price_refs)` return `NotSupported` —
  either branch in code or document that those flows are MoR-only
- [ ] You've stress-tested the cancellation UX: cancellation is always
  scheduled, so "you cancelled but you still have access until DATE" is
  the message your UI should show
- [ ] You've stress-tested the subscription-arrival webhook: there is a
  window where the customer has paid but the mirror has no row yet
- [ ] You're aggregating revenue correctly: Paddle amounts are
  tax-inclusive, Stripe amounts are tax-exclusive

## Next

- [Payments](payments.md) — the generic five-trait surface and the
  webhook handler's mirror-hydration contract
- [Payments — Frontend Integration](payments-frontend.md) — paddle.js
  inline checkout in Svelte / React / Vue
- [Payments — Provider Guide](payments-provider-guide.md) — write your
  own adapter crate end-to-end
- [Configuration](configuration.md) — typed config registration the
  Paddle env vars plug into
- [Application Bootstrap](bootstrap.md) — where
  `PaymentProviderRegistry::bind` actually lives in your app
