# Payments — Stripe Adapter

`suprnova-payments-stripe` is the reference adapter for Suprnova's
provider-neutral payments surface. It implements all five payment traits
(`Checkout`, `Payment`, `Subscription`, `CustomerStore`, `WebhookHandler`)
against the Stripe API via `async-stripe` 1.0.0-rc.5. Reach for this
chapter when you need to know exactly which Stripe endpoint a method
calls, how the webhook signature format is verified, how PaymentIntents
flow through `ChargeResult`, or which event types map onto the neutral
event enum.

For the trait shapes themselves, env-var setup, and the bootstrap
pattern, read [Payments](payments.md) first. This chapter is the
Stripe-specific deep dive.

## Gateway, not Merchant of Record

Stripe is a **payment gateway**: you receive funds directly into your
own bank account, and you are responsible for tax collection and
remittance, invoicing, dunning, and chargeback handling. Contrast with
Paddle ([Payments — Paddle](payments-paddle.md)), where Paddle is the
Merchant of Record — they collect the funds, file the tax, and pay
you out net of fees.

The practical consequence for this chapter: `StripeProvider` implements
`Payment` (you can authorise, capture, refund, and void a card on the
server). `PaddleProvider` does not. The trait split exists because the
two flows are genuinely different — not because we ran out of time.

### Why Suprnova diverges

Laravel ships Cashier as a first-party Stripe integration in the core
docs. It is convenient, but Stripe-only — and adding a second provider
means either forking Cashier or building a parallel surface.

Suprnova keeps Stripe at arm's length. The Stripe adapter is one crate
that registers itself against the same five traits any other provider
implements. Your domain code never names `StripeProvider`; it calls
`provider.charge(...)` against `Arc<dyn PaymentProvider>` resolved from
the registry, and the Stripe behaviour is one swap-out from the Paddle
behaviour. When you later add Mollie, or wire up a regional gateway
that doesn't exist yet, you implement the same five traits and the
rest of your app does not move.

## Construction

```rust
use suprnova_payments_stripe::StripeProvider;
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;

// Production: read from env.
let stripe = StripeProvider::from_env()
    .expect("STRIPE_SECRET_KEY / PUBLISHABLE_KEY / WEBHOOK_SIGNING_SECRET");

// Tests / explicit config:
let stripe = StripeProvider::new(
    "sk_test_...",
    "pk_test_...",
    "whsec_...",
);

PaymentProviderRegistry::bind("stripe", Arc::new(stripe));
```

`StripeProvider` is `Clone` (cheap — the underlying `stripe::Client` is
`Arc`-backed) and holds three values:

| Field | Source | Use |
|---|---|---|
| `secret_key` | `sk_live_…` / `sk_test_…` | HTTP `Authorization: Bearer …` on every API call |
| `publishable_key` | `pk_live_…` / `pk_test_…` | Surfaced inside `SessionPayload::StripeElements` so the frontend can mount Stripe.js without a separate config lookup |
| `webhook_signing_secret` | `whsec_…` | HMAC-SHA256 verification of the `Stripe-Signature` header |

`from_env()` returns `Result<Self, String>` — the error message names
the missing variable. There is no panic path at boot.

## The PaymentIntent lifecycle

Stripe represents a single charge attempt as a **PaymentIntent**. The
intent moves through statuses; the Suprnova `Payment` trait drives the
transitions. Every `StripeProvider` `Payment` method maps to one
`/v1/payment_intents/...` endpoint:

| `Payment` method | Stripe endpoint | What it does |
|---|---|---|
| `charge` | `POST /v1/payment_intents` | Create + confirm in one call against a saved payment method. `capture_method: "manual"` so the intent moves to `requires_capture`, **not** `succeeded`. |
| `capture` | `POST /v1/payment_intents/{id}/capture` | Settle a previously-authorised intent. Status `requires_capture` → `succeeded`. |
| `refund` | `POST /v1/refunds` | Fully or partially reverse a captured intent. |
| `void` | `POST /v1/payment_intents/{id}/cancel` | Release an authorisation before capture. Status `requires_capture` → `canceled`. |
| `status` | `GET /v1/payment_intents/{id}` | Retrieve the current status (returns `PaymentStatus`). |

### Authorise first, capture later

`StripeProvider::charge` does **not** immediately settle the funds.
It sends `capture_method=manual` + `confirm=true`, which authorises
the card and reserves the funds, then waits for an explicit `capture`
call. This is the canonical two-step flow:

```rust
use suprnova::payments::{
    PaymentProviderRegistry, ChargeRequest, ChargeResult,
    Money, Currency, PaymentStatus,
};

let provider = PaymentProviderRegistry::get("stripe").unwrap();
let payment = provider.as_payment()
    .expect("Stripe implements Payment");

let result = payment.charge(ChargeRequest {
    customer_ref: "cus_NffrFeUfNV2Hib".into(),
    payment_method_ref: "pm_card_visa".into(),
    amount: Money::from_minor_units(2999, Currency::USD),
    description: Some("Pro plan, manual capture".into()),
    idempotency_key: Some("order-12345".into()),  // see "Idempotency" below
    metadata: None,
}).await?;

match result {
    ChargeResult::Completed { provider_transaction_id, status, .. }
        if status == PaymentStatus::Pending => {
        // Authorised — settle when the order ships.
        let settled = payment.capture(&provider_transaction_id).await?;
        assert!(matches!(
            settled,
            ChargeResult::Completed { status: PaymentStatus::Succeeded, .. }
        ));
    }
    ChargeResult::RequiresClientAction { client_secret, .. } => {
        // 3DS step-up needed — see "3DS and SCA" below.
    }
    other => panic!("unexpected charge result: {other:?}"),
}
```

If you want **immediate** capture — the common e-commerce one-shot —
use `Checkout::start_session` with `SessionMode::OneOff` instead. That
path creates a PaymentIntent with `automatic_payment_methods` enabled
and hands the client secret to the frontend so the customer's browser
confirms the intent in-place. `Payment::charge` is for server-driven
flows where you already hold the customer's saved payment method and
want explicit authorise-then-capture control (typical for marketplaces,
delayed-fulfilment SaaS, or split-shipment commerce).

### Status mapping

Stripe statuses fold into Suprnova's `PaymentStatus` enum:

| `PaymentIntentStatus` | `PaymentStatus` |
|---|---|
| `Succeeded` | `Succeeded` |
| `Processing` | `Pending` |
| `RequiresCapture` | `Pending` (authorised, awaiting capture) |
| `RequiresAction` | `Pending` (returned as `RequiresClientAction` from `charge`) |
| `RequiresConfirmation` | `Pending` |
| `RequiresPaymentMethod` | `Pending` |
| `Canceled` | `Canceled` |
| _new Stripe status (enum is `#[non_exhaustive]`)_ | `Failed` |

The `non_exhaustive` fallback is intentional. Stripe occasionally adds
states (e.g. when introducing new payment method types). Surfacing them
as `Failed` is the conservative default — your app treats the charge
as not-yet-confirmed until you upgrade the adapter.

### 3DS and SCA

European Strong Customer Authentication, India's RBI rules, and
several other regulators require the cardholder to authenticate the
charge in a separate browser context. Stripe surfaces this as
`requires_action` with a `next_action` block.

`StripeProvider::charge` translates this into one of two
`ChargeResult` variants:

```rust
ChargeResult::RequiresClientAction {
    provider_transaction_id,   // pi_xxx — keep this around
    action_kind: "stripe_3ds", // Stripe-specific tag
    client_secret,             // hand to Stripe.js
    publishable_key,           // hand to Stripe.js
}
```

When the intent's `next_action` contains a redirect URL (some
authentication flows are URL-redirect rather than in-place modal),
the result is rewritten as:

```rust
ChargeResult::RedirectRequired {
    provider_transaction_id,
    url,                       // redirect the browser here
    return_to: None,
}
```

Your controller hands the `RequiresClientAction` payload to the
Inertia page; the frontend calls `stripe.confirmCardPayment(client_secret, ...)`
and the customer completes 3DS. When confirmation succeeds, Stripe
fires `payment_intent.succeeded` and the webhook route writes the
mirror row. See [Payments — Frontend Integration](payments-frontend.md)
for the Svelte / React / Vue snippets.

### Void vs refund

`void` releases an authorisation **before** capture; `refund` reverses
a captured payment. Calling `void` on a captured intent will fail —
Stripe rejects with a message containing `"already succeeded"` or
`"You cannot cancel"`, and the adapter surfaces that as
`PaymentError::Validation` so your handler can distinguish a
recoverable user error (use `refund` instead) from a true provider
outage. Any other failure is `PaymentError::Provider`.

```rust
let voided = payment.void("pi_3PNzj...").await;
match voided {
    Ok(()) => { /* authorisation released */ }
    Err(suprnova::payments::PaymentError::Validation(msg)) => {
        // Already captured — call refund instead.
        let refund = payment.refund(RefundRequest {
            provider_transaction_id: "pi_3PNzj...".into(),
            amount: None,           // full refund
            reason: Some("requested_by_customer".into()),
            idempotency_key: None,  // refund() does not forward this — see "Idempotency"
        }).await?;
    }
    Err(e) => return Err(e.into()),
}
```

## Customers

`StripeProvider` implements `CustomerStore` against
`/v1/customers`. The adapter maps a returned `Customer` into the
neutral `CustomerRef`, preserving the email and your application's
`user_id`:

```rust
use suprnova::payments::CreateCustomerRequest;

let customer = provider.create_customer(CreateCustomerRequest {
    user_id: "user-42".into(),       // your app's user id
    email: "alice@example.com".into(),
    name: Some("Alice Example".into()),
    metadata: None,
}).await?;

// customer.provider_customer_id == "cus_NffrFeUfNV2Hib"
// Persist this alongside your User row so subsequent
// charges, subscriptions, and webhooks resolve back.
```

`update_customer`, `get_customer`, and `delete_customer` hit
`POST /v1/customers/{id}`, `GET /v1/customers/{id}`, and
`DELETE /v1/customers/{id}` respectively. Stripe's delete returns a
`DeletedCustomer` envelope which the adapter discards — only the
success/failure of the call is propagated.

## Subscriptions

`StripeProvider::subscribe` posts to `/v1/subscriptions` with the
customer ref, an `items[]` array, and an optional `trial_period_days`:

```rust
use suprnova::payments::{SubscribeRequest, SubscriptionStatus};

let sub = provider.subscribe(SubscribeRequest {
    customer_ref: "cus_NffrFeUfNV2Hib".into(),
    price_refs: vec!["price_pro_monthly".into()],
    trial_days: Some(14),
    idempotency_key: None,
    metadata: None,
}).await?;

assert!(matches!(
    sub.status,
    SubscriptionStatus::Trialing | SubscriptionStatus::Active
));

println!("Period ends at {}", sub.current_period_end);
for item in &sub.items {
    println!(
        "  {} × {} @ {:?}",
        item.quantity, item.provider_price_id, item.unit_amount,
    );
}
```

### Period boundaries

Stripe moved the `current_period_start` / `current_period_end`
timestamps from the parent Subscription onto each `SubscriptionItem`
in API version `2023-08-16`. Multi-item subscriptions can in theory
have divergent item periods, but in practice every item on a single
subscription shares the parent's billing cycle. The adapter takes the
**first item's** period as the parent period in the returned
`SubscriptionResult`. If you genuinely need per-item periods, read them
from `sub.items[n]` — they are preserved on the snapshot.

### Cancel at period end vs immediately

```rust
// Soft cancel — keep access until current_period_end:
let sub = provider.cancel("sub_1234", /* at_period_end */ true).await?;
// sub.cancel_at_period_end == true
// sub.status == Active

// Immediate cancel — Stripe DELETE /v1/subscriptions/{id}:
let sub = provider.cancel("sub_1234", /* at_period_end */ false).await?;
// sub.status == Canceled
```

The two paths hit different Stripe endpoints. Soft cancel is
`POST /v1/subscriptions/{id}` with `cancel_at_period_end=true` — the
subscription stays active until the end of the billing period, then
Stripe finalises it. Immediate cancel is `DELETE /v1/subscriptions/{id}`
with `prorate=false` and `invoice_now=false`.

### `update()` is intentionally limited

`UpdateSubscriptionRequest` has two fields the adapter acts on:
`cancel_at_period_end` and `new_price_refs`. The first is supported;
the second returns `PaymentError::NotSupported`:

```rust
provider.update(UpdateSubscriptionRequest {
    provider_subscription_id: "sub_1234".into(),
    new_price_refs: Some(vec!["price_team_yearly".into()]),
    cancel_at_period_end: None,
    idempotency_key: None,
}).await
// → Err(PaymentError::NotSupported(
//      "Stripe price-set replacement on existing subscription not in v1. \
//       Cancel the subscription and create a new one with the new price set."
//   ))
```

This is one of the few places `NotSupported` is the honest answer
rather than a deferral. Stripe price-set replacement requires deleting
and re-creating subscription items — the shape varies by provider
(proration, billing-cycle anchoring, retained-trial behaviour) and
collapsing it into a single neutral API would hide more than it
helped. The recommended path is to cancel the existing subscription
and `subscribe` again with the new price set, applying your own
proration policy if you need one.

## Webhooks

Stripe sends webhooks signed with HMAC-SHA256 in the format:

```
Stripe-Signature: t=1717000000,v1=5257a869e7ecebeda32affa62cdca3fa51cad7e77a0e56ff536d0ce8e108d8bd
```

`StripeProvider::verify` parses the header, recomputes
HMAC-SHA256 over `"{timestamp}.{raw_body}"` using the webhook signing
secret, and does a **constant-time** comparison against every `v1=`
value in the header. Multiple `v1=` values exist during signing-secret
rotation — Stripe overlaps the old and new secrets for a window so
you can re-sign and deploy without a flag-day cutover.

```
Stripe-Signature: t=1717000000,v1=<old_sig>,v1=<new_sig>
```

The adapter accepts the request if **any** `v1=` value matches. A
header missing `t=` or with no `v1=` values is rejected as
`PaymentError::WebhookSignature`. Non-ASCII bytes anywhere in the
header are also rejected — Stripe never sends them, and treating them
as invalid is safer than substituting a replacement character.

You never call `verify` directly. The framework's
`webhook_routes(db.clone())` registers
`POST /webhooks/payments/{provider}` and invokes the adapter's
`verify` + `parse_event` + payload extractors for every request that
lands there. See [Idempotency](idempotency.md) for the retry-aware
audit behaviour — including the rule that previously-failed events
re-attempt hydration when the provider retries.

### Event → neutral mapping

Stripe event types map onto Suprnova's `NeutralEventKind` via the
`stripe_event_to_neutral` function. The mapping table:

| Stripe event type | `NeutralEventKind` |
|---|---|
| `payment_intent.succeeded` | `PaymentSucceeded` |
| `payment_intent.payment_failed` | `PaymentFailed` |
| `charge.refunded` | `PaymentRefunded` |
| `charge.dispute.created` | `PaymentDisputed` |
| `customer.subscription.created` | `SubscriptionCreated` |
| `customer.subscription.updated` | `SubscriptionUpdated` |
| `customer.subscription.deleted` | `SubscriptionCanceled` |
| `customer.subscription.paused` | `SubscriptionUpdated` |
| `customer.subscription.resumed` | `SubscriptionUpdated` |
| `customer.subscription.trial_will_end` | `SubscriptionUpdated` |
| `invoice.payment_succeeded` / `invoice.paid` | `InvoicePaid` |
| `invoice.payment_failed` | `InvoiceFailed` |
| `customer.created` | `CustomerCreated` |
| `customer.updated` | `CustomerUpdated` |
| _anything else_ | `None` |

Events that map to `None` (Radar fraud signals, payouts, balance
transfers, dispute lifecycle events past `created`) are still
persisted to the `payments_webhook_events` audit table — they just do
not drive the mirror tables. If you need them, read directly from
`event.raw_payload` in a custom handler.

The mapping is also re-exported at the crate root so you can use it
outside the webhook route:

```rust
use suprnova_payments_stripe::stripe_event_to_neutral;
use suprnova::payments::NeutralEventKind;

assert_eq!(
    stripe_event_to_neutral("payment_intent.succeeded"),
    Some(NeutralEventKind::PaymentSucceeded),
);
assert_eq!(
    stripe_event_to_neutral("radar.early_fraud_warning.created"),
    None,
);
```

### Payload extraction

After `verify` and `parse_event` succeed, the framework calls
`extract_payload_ids`, `extract_payment_snapshot`, and
`extract_customer_snapshot` to pull the fields that drive the mirror
tables (see [Eloquent](eloquent.md) for the underlying
read-from-your-own-DB pattern). Stripe is structurally consistent:
every webhook puts the relevant entity at `data.object`, with `id` as
its primary key.

The extractors handle four event families:

- **Subscription events** — pull `data.object.id` (the subscription
  id) and `data.object.customer`.
- **Customer events** — pull `data.object.id` (the customer id).
- **PaymentIntent / Charge events** — pull `data.object.id`,
  `data.object.amount`, `data.object.currency`, `data.object.customer`,
  and (for `payment_intent.succeeded` only) `data.object.created` as
  `paid_at`.
- **Invoice events** — pull `data.object.id`, the customer pointer,
  `data.object.subscription` (recurring charges only), `amount_paid`
  (falling back to `amount_due`), `tax`, `currency`, and
  `data.object.status_transitions.paid_at`.

Anything else returns `None` from the snapshot extractors; the audit
row still lands.

## Mirror tables

Six tables back the payments surface in your application's database.
Apply the framework migration alongside your own:

```rust
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use suprnova::payments::migrations::CreatePaymentsTables;

pub struct Migrator;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            // ... your migrations ...
            Box::new(CreatePaymentsTables),
        ]
    }
}
```

The tables created are `payments_customers`, `payments_payment_methods`,
`payments_subscriptions`, `payments_subscription_items`,
`payments_transactions`, and `payments_webhook_events`. The webhook
route hydrates them inside a single DB transaction per event — partial
state is never observable, and the audit row carries
`process_error` across retries so failures stay visible to operators.

## Idempotency

Outbound idempotency on Stripe API calls and inbound idempotency on
webhook deliveries are two separate stories. Read them as such.

### Outbound: per-method coverage

Stripe supports request idempotency via the `Idempotency-Key` HTTP
request header — the same key with the same body returns the same
response object for a 24-hour replay window; a mismatched body returns
an error. The Suprnova Stripe adapter does **not** uniformly thread the
DTO's `idempotency_key` field onto that header today. The actual
behaviour as of this writing:

| Method | DTO field | What the adapter does |
|---|---|---|
| `Payment::charge` | `ChargeRequest::idempotency_key` | Forwarded into the POST body as `idempotency_key=...` (not the HTTP header). Stripe's API does **not** read body-form idempotency keys, so this is best treated as not effective until the adapter migrates to the request-header path. |
| `Payment::refund` | `RefundRequest::idempotency_key` | Silently discarded — the field is not forwarded. |
| `Checkout::start_session` | `StartSessionRequest::idempotency_key` | Silently discarded. |
| `Subscription::subscribe` / `update` | `*Request::idempotency_key` | Silently discarded. |

If you rely on at-most-once semantics for charge/refund retries
against Stripe today, gate the retry at your own call site (a
deterministic domain key persisted in your DB, with a unique index
preventing the second insert) until the adapter wires the header
through. The DTO fields are accepted on the API but not currently
honoured all the way to the wire — set them to `None` in tests and
production code so the gap is explicit, and don't assume Stripe is
deduplicating your retries.

This is a known gap in the v1 adapter and a candidate fix for the
next release; the surface shape stays the same once the wiring lands.

### Inbound: webhook deduplication

Webhook idempotency is handled by the framework on the ingress side
and is fully wired. Every event lands in `payments_webhook_events`
with a UNIQUE index on `(provider, provider_event_id)`. Duplicate
deliveries of an event that was already processed return 200 to
Stripe immediately without re-running hydration; duplicates of a
previously **failed** event re-attempt hydration so the provider's
retry is your recovery mechanism. See [Idempotency](idempotency.md)
for the full audit + retry contract.

## Testing

The adapter is hyper-backed and rustls-fronted. Tests that construct
a `StripeProvider` need a registered crypto provider; we install
`ring` exactly once in `#[cfg(test)]`:

```rust
#[cfg(test)]
mod tests {
    use suprnova_payments_stripe::StripeProvider;
    use std::sync::OnceLock;

    fn install_crypto_provider() {
        static ONCE: OnceLock<()> = OnceLock::new();
        ONCE.get_or_init(|| {
            let _ = rustls::crypto::ring::default_provider().install_default();
        });
    }

    fn provider() -> StripeProvider {
        install_crypto_provider();
        StripeProvider::new("sk_test_dummy", "pk_test_dummy", "whsec_dummy")
    }

    #[test]
    fn parses_subscription_webhook_ids() {
        let p = provider();
        let event = /* construct WebhookEvent with raw_payload */;
        let ids = p.extract_payload_ids(&event);
        assert_eq!(ids.subscription_id.as_deref(), Some("sub_abc"));
    }
}
```

For integration tests that hit the live Stripe sandbox, set
`STRIPE_SECRET_KEY` and friends in your test env. For unit tests of
your own controllers, prefer `MockPaymentProvider` from the framework
— it implements all five traits with predictable returns and zero
network.

## Next

- [Payments](payments.md) — the trait surface, the registry, the
  bootstrap pattern, and the flow-tagged `SessionPayload`.
- [Payments — Paddle](payments-paddle.md) — the Merchant-of-Record
  counterpart; same five traits, different responsibility split.
- [Payments — Provider Guide](payments-provider-guide.md) — how to
  write an adapter for a gateway Suprnova doesn't ship.
- [Payments — Frontend Integration](payments-frontend.md) — Svelte /
  React / Vue dispatch on `SessionPayload.flow`, including the
  Stripe.js confirm-card-payment loop.
- [Idempotency](idempotency.md) — the audit + retry contract that
  makes webhook handling safe under at-least-once delivery.
- [Eloquent](eloquent.md) — query the mirror tables alongside your
  own models; everything is just a SeaORM entity.
