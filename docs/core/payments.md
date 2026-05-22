# Payments

Suprnova's payments surface is provider-neutral. You pick an adapter crate — Stripe, Paddle, or one you write yourself — register it at boot, and your domain code calls the same five traits regardless of which provider is behind it. Mirror tables in your database are kept in sync by webhooks, so your domain code reads from your own DB rather than hitting the provider API for every query.

No feature is gated to a single provider. Stripe's direct-capture model and Paddle's Merchant-of-Record model both fit into the same trait contract. The only surface that differs is `Payment` (server-side capture), which is optional — Paddle doesn't need it, so Paddle doesn't implement it.

## Quick Start

**`Cargo.toml`:**

```toml
[dependencies]
suprnova-payments-stripe = "0.1"
```

**`src/bootstrap.rs`:**

```rust,ignore
use std::sync::Arc;
use suprnova::payments::{PaymentProviderRegistry, webhook_routes};
use suprnova_payments_stripe::StripeProvider;

pub async fn bootstrap(app: &mut App, db: Arc<DatabaseConnection>) {
    let stripe = StripeProvider::from_env().expect("Stripe env vars not set");
    PaymentProviderRegistry::bind("stripe", Arc::new(stripe));

    let router = webhook_routes(db.clone());
    app.merge_router(router);
}
```

**`src/controllers/billing.rs`:**

```rust,ignore
use std::sync::Arc;
use suprnova::payments::*;

pub async fn start_checkout(
    provider: Arc<dyn PaymentProvider>,
    user_id: String,
    email: String,
) -> PaymentResult<SessionPayload> {
    let customer = provider.create_customer(CreateCustomerRequest {
        user_id,
        email,
        name: None,
        metadata: None,
    }).await?;

    provider.start_session(StartSessionRequest {
        mode: SessionMode::Subscription,
        customer_ref: customer.provider_customer_id,
        price_refs: vec!["price_pro_monthly".into()],
        success_return_url: "https://app.example/billing/success".into(),
        cancel_return_url: "https://app.example/billing/cancel".into(),
        amount_hint: None,
        idempotency_key: None,
        metadata: None,
    }).await
}
```

That `SessionPayload` goes into your Inertia page props. The frontend dispatches on `payload.flow` to render the right widget — see [`payments-frontend.md`](./payments-frontend.md).

## Picking an Adapter

### Stripe

```bash
cargo add suprnova-payments-stripe
```

Required env vars:

| Variable | Description |
|---|---|
| `STRIPE_SECRET_KEY` | Secret key (`sk_live_…` / `sk_test_…`) |
| `STRIPE_PUBLISHABLE_KEY` | Publishable key (`pk_live_…` / `pk_test_…`) |
| `STRIPE_WEBHOOK_SIGNING_SECRET` | Webhook endpoint signing secret (`whsec_…`) |

```rust,ignore
use suprnova_payments_stripe::StripeProvider;
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;

// From env (recommended in production):
let stripe = StripeProvider::from_env().expect("Stripe env vars not set");

// Or construct directly:
let stripe = StripeProvider::new("sk_test_...", "pk_test_...", "whsec_...");

PaymentProviderRegistry::bind("stripe", Arc::new(stripe));
```

Stripe implements all five traits including `Payment` (server-side capture via PaymentIntents). Calling `provider.as_payment()` returns `Some`.

### Paddle

```bash
cargo add suprnova-payments-paddle
```

Required env vars:

| Variable | Description |
|---|---|
| `PADDLE_API_KEY` | API key (`pdl_live_apikey_…` / `pdl_sdbx_apikey_…`) |
| `PADDLE_WEBHOOK_KEY` | Notification destination secret (`pdl_ntfset_…`) |
| `PADDLE_CLIENT_TOKEN` | Client-side token (`live_…` / `test_…`) |
| `PADDLE_ENVIRONMENT` | Optional, defaults to `"sandbox"` |

```rust,ignore
use suprnova_payments_paddle::{PaddleProvider, PaddleEnvironment};
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;

// From env:
let paddle = PaddleProvider::from_env().expect("Paddle env vars not set");

// Or construct directly:
let paddle = PaddleProvider::new(
    "pdl_sdbx_apikey_...",
    "pdl_ntfset_...",
    "test_...",
    PaddleEnvironment::Sandbox,
).expect("Paddle client init failed");

PaymentProviderRegistry::bind("paddle", Arc::new(paddle));
```

Paddle is a Merchant of Record — it manages tax, dunning, and the full subscription lifecycle. It does not expose server-side capture, so `Payment` is not implemented. Calling `provider.as_payment()` returns `None`. Subscriptions are created indirectly: call `Checkout::start_session`, complete the Paddle widget, and the `SubscriptionCreated` webhook arrives to confirm the subscription ID.

## The Five Traits

### `Checkout` — universal, opens the client widget

Every provider implements `Checkout`. Call `start_session` to get a flow-tagged `SessionPayload` that your frontend renders.

```rust,ignore
#[async_trait]
pub trait Checkout: Send + Sync {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload>;
}
```

`StartSessionRequest` fields:

| Field | Type | Description |
|---|---|---|
| `mode` | `SessionMode` | `OneOff` or `Subscription` |
| `customer_ref` | `String` | Provider customer ID from `CustomerStore::create_customer` |
| `price_refs` | `Vec<String>` | Provider price/product IDs |
| `success_return_url` | `String` | Where to send the user after payment |
| `cancel_return_url` | `String` | Where to send the user if they abandon |
| `amount_hint` | `Option<Money>` | Override or hint for one-off amounts |
| `idempotency_key` | `Option<String>` | For safe retries |

### `Payment` — optional, server-side capture

Only providers that expose server-side capture implement `Payment`. Stripe does; Paddle does not. To check at runtime:

```rust,ignore
let provider = PaymentProviderRegistry::get("stripe").unwrap();
if let Some(payment) = provider.as_payment() {
    let result = payment.charge(ChargeRequest {
        customer_ref: "cus_...".into(),
        payment_method_ref: "pm_...".into(),
        amount: Money::from_minor_units(2999, Currency::USD),
        description: Some("Pro plan one-off".into()),
        idempotency_key: Some("charge_user42_order99".into()),
        metadata: None,
    }).await?;
}
```

Full `Payment` interface:

```rust,ignore
#[async_trait]
pub trait Payment: Send + Sync {
    async fn charge(&self, req: ChargeRequest) -> PaymentResult<ChargeResult>;
    async fn capture(&self, provider_transaction_id: &str) -> PaymentResult<ChargeResult>;
    async fn refund(&self, req: RefundRequest) -> PaymentResult<RefundResult>;
    async fn void(&self, provider_transaction_id: &str) -> PaymentResult<()>;
    async fn status(&self, provider_transaction_id: &str) -> PaymentResult<PaymentStatus>;
}
```

`ChargeResult` is an enum tagged with `kind` — see the [Money and ChargeResult](#chargeresult) section.

### `Subscription` — subscribe, update, cancel, get

```rust,ignore
#[async_trait]
pub trait Subscription: Send + Sync {
    async fn subscribe(&self, req: SubscribeRequest) -> PaymentResult<SubscriptionResult>;
    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult>;
    async fn cancel(&self, provider_subscription_id: &str, at_period_end: bool) -> PaymentResult<SubscriptionResult>;
    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult>;
}
```

Cancel at period end (keeps access until billing cycle ends):

```rust,ignore
let sub = provider.cancel(&sub_id, true).await?;
// sub.cancel_at_period_end == true, sub.status == Active

// Cancel immediately:
let sub = provider.cancel(&sub_id, false).await?;
// sub.status == Canceled
```

Note: `Paddle::subscribe` returns `PaymentError::NotSupported` — Paddle creates subscriptions through checkout completion, not direct API calls. Use `Checkout::start_session` and wait for the `SubscriptionCreated` webhook.

### `CustomerStore` — create, update, get, delete

```rust,ignore
#[async_trait]
pub trait CustomerStore: Send + Sync {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef>;
    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef>;
    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef>;
    async fn delete_customer(&self, provider_customer_id: &str) -> PaymentResult<()>;
}
```

`CreateCustomerRequest` takes `user_id`, `email`, `name: Option<String>`, and `metadata: Option<Value>`. `CustomerRef` comes back with `provider_customer_id` — store that alongside your user record to use in subsequent calls.

### `WebhookHandler` — verify signature and parse event

```rust,ignore
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()>;
    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent>;
}
```

In practice you never call these directly — `webhook_routes` calls them automatically for every inbound webhook. They are part of the trait so adapter crates can implement provider-specific signature verification and event parsing in a testable way.

## The Flow-Tagged Inertia Payload

`start_session` returns a `SessionPayload` enum that serializes to JSON with a `flow` discriminator field. Your frontend switches on `flow` to render the right widget:

```rust,ignore
#[serde(tag = "flow", rename_all = "snake_case")]
pub enum SessionPayload {
    StripeElements {
        client_secret: String,
        publishable_key: String,
        provider_session_id: String,
    },
    StripeCheckoutRedirect {
        url: String,
        provider_session_id: String,
    },
    PaddleInline {
        transaction_id: String,
        customer_token: Option<String>,
        client_token: String,
    },
    Redirect {
        url: String,
        provider_session_id: String,
    },
}
```

Serialized form of a `StripeElements` payload:

```json
{
  "flow": "stripe_elements",
  "client_secret": "pi_..._secret_...",
  "publishable_key": "pk_live_...",
  "provider_session_id": "pi_..."
}
```

Return this from your controller as Inertia props. Frontend integration is described in [`payments-frontend.md`](./payments-frontend.md).

## Mirror Tables

Six tables are created by the framework migration. Include the migration in your app's migrator:

```rust,ignore
use suprnova::payments::migrations::m_2026_05_22_000001_create_payments_tables::Migration as PaymentsMigration;

impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            // ... your other migrations ...
            Box::new(PaymentsMigration),
        ]
    }
}
```

### Table overview

| Table | Purpose |
|---|---|
| `payments_customers` | One row per `(provider, user_id)` pair |
| `payments_payment_methods` | Stored payment methods per customer |
| `payments_subscriptions` | Subscription lifecycle state |
| `payments_subscription_items` | Line items within a subscription |
| `payments_transactions` | One-off charges and subscription invoices |
| `payments_webhook_events` | Audit log and idempotency guard |

Every table has a `provider_metadata` JSON column. When the framework's neutral representation doesn't cover a provider-specific field, read it from there.

### Transactions table

`payments_transactions` splits amounts into `amount_total_minor` and `amount_tax_minor`. Stripe reports amounts exclusive of tax — tax is zero on the transaction row, and any tax data lives in `provider_metadata`. Paddle reports amounts inclusive of tax and sets `amount_tax_minor` to the tax component. Both representations work; add `amount_total_minor - amount_tax_minor` for the net amount.

### Webhook events table

`payments_webhook_events` has a `UNIQUE(provider, provider_event_id)` index. Every inbound webhook is checked against this before processing — duplicates return 200 OK without re-processing. This is load-bearing: Stripe, Paddle, and most providers retry failed webhooks aggressively.

### Caveats

Domain code reads from the mirror tables, not directly from the provider API. Mutations (create subscription, cancel, etc.) go to the provider; the resulting webhook syncs the mirror tables back. This means there is a brief window between a mutation and the webhook arriving where your mirror tables lag behind. Design your UX to account for this (show "processing" states, rely on the provider's redirect URLs for immediate confirmation).

## Webhook Handling

Mount the webhook ingress route once at bootstrap. The handler for `POST /webhooks/payments/{provider}` is built into the framework:

```rust,ignore
use std::sync::Arc;
use suprnova::payments::webhook_routes;

// In your router setup:
let router = webhook_routes(db.clone());
app.merge_router(router);
```

The framework handler does this for each request:

1. Looks up the named provider in `PaymentProviderRegistry`.
2. Calls `WebhookHandler::verify` to check the signature. Returns 401 on failure.
3. Calls `WebhookHandler::parse_event` to build a `WebhookEvent`. Returns 400 on parse failure.
4. Checks `payments_webhook_events` for an existing row with the same `(provider, provider_event_id)`. If found, returns 200 immediately — this is the idempotency guard.
5. Inserts the audit row.

### WebhookEvent structure

```rust,ignore
pub struct WebhookEvent {
    pub provider: String,
    pub provider_event_id: String,
    pub provider_event_type: String,        // raw provider string, e.g. "customer.subscription.created"
    pub neutral: Option<NeutralEventKind>,  // mapped to framework taxonomy, or None for provider-specific events
    pub raw_payload: Value,                 // full JSON body for fallthrough
}
```

`NeutralEventKind` covers the common path:

```rust,ignore
pub enum NeutralEventKind {
    PaymentSucceeded,
    PaymentFailed,
    PaymentRefunded,
    PaymentDisputed,
    SubscriptionCreated,
    SubscriptionUpdated,
    SubscriptionCanceled,
    InvoicePaid,
    InvoiceFailed,
    CustomerCreated,
    CustomerUpdated,
}
```

When `neutral` is `None`, the event is provider-specific. Read `provider_event_type` and `raw_payload` for the full data.

## Money

Amounts are represented as `Money` — an `i64` minor-unit count plus a `Currency`. No `f64` involved.

```rust,ignore
use suprnova::payments::{Money, Currency};
use rust_decimal::Decimal;
use std::str::FromStr;

// From minor units (cents, pence, yen, etc.)
let price = Money::from_minor_units(1999, Currency::USD);  // $19.99

// From a decimal string
let price = Money::from_decimal(Decimal::from_str("19.99").unwrap(), Currency::USD);

// Zero-decimal currencies — 1234 minor = 1234 JPY (no conversion)
let yen = Money::from_minor_units(1234, Currency::JPY);

// Arithmetic — panics on currency mismatch
let total = price + Money::from_minor_units(100, Currency::USD);  // $20.99

// Negative values represent refunds or credits
let refund = Money::from_minor_units(-500, Currency::USD);  // -$5.00

// Read back
println!("{} minor units in {:?}", price.minor_units(), price.currency());
```

`Add` and `Sub` panic on currency mismatch and on `i64` overflow. Use the panicking arithmetic for correctness — silent cross-currency addition is a bug, not a feature.

## ChargeResult

`Payment::charge` returns a `ChargeResult` enum. Not every charge completes immediately — 3DS step-up and off-session cards can require a redirect or a client-side action:

```rust,ignore
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ChargeResult {
    Completed {
        provider_transaction_id: String,
        amount: Money,
        status: PaymentStatus,
        provider_metadata: Value,
    },
    RedirectRequired {
        provider_transaction_id: String,
        url: String,
        return_to: Option<String>,
    },
    RequiresClientAction {
        provider_transaction_id: String,
        action_kind: String,
        client_secret: Option<String>,
        publishable_key: Option<String>,
    },
}
```

Handle `RequiresClientAction` by returning the payload to your frontend. The frontend renders the 3DS challenge using `client_secret` + `publishable_key`. See [`payments-frontend.md`](./payments-frontend.md) for the frontend dispatch code.

## Idempotency Keys

Every mutating DTO has an optional `idempotency_key: Option<String>`. Set one on retryable network calls:

```rust,ignore
provider.start_session(StartSessionRequest {
    // ...
    idempotency_key: Some(format!("checkout_{}_{}", user_id, order_id)),
    // ...
}).await?;

provider.subscribe(SubscribeRequest {
    // ...
    idempotency_key: Some(format!("sub_{}_{}", user_id, plan_id)),
    // ...
}).await?;
```

Stripe honors idempotency keys via the `Idempotency-Key` HTTP header. Paddle has an equivalent mechanism. If a request fails mid-flight and you retry with the same key, the provider returns the original response instead of creating a duplicate charge or subscription.

## The Discriminator Pattern

Every adapter that claims to implement `PaymentProvider` must pass the same E2E flow:

```
create_customer → start_session → subscribe → get → cancel(at_period_end) → cancel(immediate) → assert as_payment invariant
```

The `MockPaymentProvider` included with the framework passes this:

```rust,ignore
use suprnova::payments::*;

#[tokio::test]
async fn discriminator_flow() {
    let provider = MockPaymentProvider::new();

    let cus = provider.create_customer(CreateCustomerRequest {
        user_id: "user_42".into(),
        email: "alice@example.com".into(),
        name: Some("Alice".into()),
        metadata: None,
    }).await.unwrap();

    let session = provider.start_session(StartSessionRequest {
        mode: SessionMode::Subscription,
        customer_ref: cus.provider_customer_id.clone(),
        price_refs: vec!["price_pro_monthly".into()],
        success_return_url: "https://app.example/billing/success".into(),
        cancel_return_url: "https://app.example/billing/cancel".into(),
        amount_hint: None,
        idempotency_key: Some("idem_1".into()),
        metadata: None,
    }).await.unwrap();
    assert!(matches!(session, SessionPayload::Redirect { .. }));

    let sub = provider.subscribe(SubscribeRequest {
        customer_ref: cus.provider_customer_id.clone(),
        price_refs: vec!["price_pro_monthly".into()],
        trial_days: None,
        idempotency_key: Some("idem_2".into()),
        metadata: None,
    }).await.unwrap();
    assert_eq!(sub.status, SubscriptionStatus::Active);

    // Cancel at period end
    let s = provider.cancel(&sub.provider_subscription_id, true).await.unwrap();
    assert!(s.cancel_at_period_end);

    // Cancel immediately
    let s = provider.cancel(&sub.provider_subscription_id, false).await.unwrap();
    assert_eq!(s.status, SubscriptionStatus::Canceled);

    // MockPaymentProvider deliberately omits Payment (Paddle-style optional)
    let p: &dyn PaymentProvider = &provider;
    assert!(p.as_payment().is_none());
}
```

`MockPaymentProvider` does not implement `Payment` — this exercises the same invariant as Paddle. `StripeProvider` and `PaddleProvider` both pass the same flow against the live API in integration tests.

## Multi-Provider Apps

Register both adapters at boot and dispatch based on where each customer's record was created:

```rust,ignore
PaymentProviderRegistry::bind("stripe", Arc::new(stripe_provider));
PaymentProviderRegistry::bind("paddle", Arc::new(paddle_provider));

// Later, per request:
let provider_name = user.payment_provider.as_str(); // "stripe" or "paddle"
let provider = PaymentProviderRegistry::get(provider_name).expect("unknown provider");
let sub = provider.cancel(&sub_id, true).await?;
```

Common uses: route EU customers through Paddle (for MoR tax handling) and US customers through Stripe; A/B test checkout conversion between providers; use one provider for subscriptions and another for one-off charges.

## Migration from Laravel Cashier

Cashier is Stripe-only by design. Suprnova ships multi-provider out of the box. Quick mapping:

| Laravel Cashier | Suprnova |
|---|---|
| `$user->newSubscription('default', 'price_pro')->create()` | `provider.subscribe(SubscribeRequest { ... }).await` |
| `$user->subscription('default')->cancel()` | `provider.cancel(&sub_id, true).await` |
| `Cashier::webhookHandler` | `webhook_routes(db.clone())` |
| `$user->createAsStripeCustomer()` | `provider.create_customer(CreateCustomerRequest { ... }).await` |
| `$user->charge(1999, 'pm_...')` | `payment.charge(ChargeRequest { ... }).await` (if provider supports it) |
| `$invoice->download()` | Not built-in; read `provider_metadata["invoice_pdf_url"]` from the transactions mirror table |

## What's Next

- [`payments-provider-guide.md`](./payments-provider-guide.md) — write your own adapter crate end to end
- [`payments-frontend.md`](./payments-frontend.md) — Svelte 5, React 19, and Vue 3.5 dispatch-on-flow examples
