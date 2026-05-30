# Payments

Suprnova's payments surface is provider-neutral. You pick an adapter crate — Stripe, Paddle, or one you write yourself — register it at boot, and your domain code calls the same four core traits (plus an optional fifth for server-side capture) regardless of which provider is behind it. Mirror tables in your database are kept in sync by webhooks, so your domain code reads from your own DB rather than hitting the provider API for every query.

No feature is gated to a single provider. Stripe's direct-capture model and Paddle's Merchant-of-Record model both fit into the same trait contract. The only surface that differs is `Payment` (server-side capture), which is optional — Paddle doesn't need it, so Paddle doesn't implement it. Providers advertise their capability by overriding `PaymentProvider::as_payment()` to return `Some(&dyn Payment)`; callers query at runtime.

## Why Suprnova diverges

Laravel ships Cashier as a first-party Stripe integration in the core docs. It's convenient, but Stripe-only — adding a second provider means forking Cashier or building a parallel surface. Suprnova treats payment providers the way it treats cache and storage drivers: one generic trait set, swappable adapters. Your domain code never names `StripeProvider` or `PaddleProvider`; it calls `provider.subscribe(...)` against `Arc<dyn PaymentProvider>` resolved from a registry, and the provider behind it is one bootstrap change away from being something else.

## Quick start

Add the adapter crate. Until Suprnova ships its v0.1 release, the framework and its adapter crates are consumed by git rather than crates.io:

```toml
# Cargo.toml
[dependencies]
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
suprnova-payments-stripe = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

Register the provider and the webhook router at boot. The webhook router is a regular `Router` you compose into your `routes::register()`:

```rust,ignore
// src/bootstrap.rs
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;
use suprnova_payments_stripe::StripeProvider;

pub async fn register() {
    let stripe = StripeProvider::from_env().expect("Stripe env vars not set");
    PaymentProviderRegistry::bind("stripe", Arc::new(stripe));
}
```

```rust,ignore
// src/routes.rs
use std::sync::Arc;
use suprnova::payments::webhook_routes;
use suprnova::container::App;
use suprnova::Router;
use sea_orm::DatabaseConnection;

/// `Application::routes(routes::register)` calls this once at boot.
/// We start from the payments webhook router, then layer the rest of
/// the app's routes on top with normal `.get(...)` / `.post(...)` calls.
pub fn register() -> Router {
    let db: Arc<DatabaseConnection> = App::get().expect("db not bound");

    webhook_routes(db)
        .get("/", crate::controllers::home::index)
        .post("/login", crate::controllers::auth::login)
        // ... the rest of your routes ...
        .into()
}
```

`webhook_routes(db)` returns a `Router` containing just `POST /webhooks/payments/{provider}`. Because `Router::get` and `Router::post` each return a `RouteBuilder` that converts back to `Router` via `.into()`, chaining on top of the payments router is the most direct way to compose. If you already use the `routes!{}` macro for your normal routes, drop the webhook POST into the same block — `webhook_routes` is a convenience wrapper around one `Router::new().post(...)` call.

In your controller, look up the provider, create a customer, and open a checkout session:

```rust,ignore
// src/controllers/billing.rs
use std::sync::Arc;
use suprnova::payments::*;

pub async fn start_checkout(
    user_id: String,
    email: String,
) -> PaymentResult<SessionPayload> {
    let provider = PaymentProviderRegistry::get("stripe")
        .ok_or_else(|| PaymentError::Internal("stripe not registered".into()))?;

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

That `SessionPayload` goes into your Inertia page props. The frontend dispatches on `payload.flow` to render the right widget — see [Payments — Frontend Integration](payments-frontend.md).

## Picking an adapter

### Stripe

```toml
# Cargo.toml
suprnova-payments-stripe = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
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

```toml
# Cargo.toml
suprnova-payments-paddle = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
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

## The trait split

`PaymentProvider` is an umbrella that bundles four universal traits — `Checkout`, `Subscription`, `CustomerStore`, `WebhookHandler` — every adapter implements. The fifth trait, `Payment`, is optional: server-side capture only makes sense for gateways like Stripe. Adapters that also implement `Payment` opt in by overriding `PaymentProvider::as_payment()`.

```rust,ignore
pub trait PaymentProvider: Checkout + Subscription + CustomerStore + WebhookHandler {
    fn name(&self) -> &'static str;

    /// Returns `Some` if this provider also implements `Payment` (server-capture).
    /// Default returns `None`.
    fn as_payment(&self) -> Option<&dyn Payment> {
        None
    }
}
```

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

### `WebhookHandler` — verify, parse, and extract

```rust,ignore
#[async_trait]
pub trait WebhookHandler: Send + Sync {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()>;
    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent>;

    /// Pull entity IDs out of the raw payload so the framework knows which
    /// mirror rows to hydrate. Default returns an empty `PayloadIds`.
    fn extract_payload_ids(&self, event: &WebhookEvent) -> PayloadIds;

    /// Build a `PaymentSnapshot` from a payment / invoice event. Default
    /// returns `None`, which skips the `payments_transactions` upsert.
    fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot>;

    /// Build a `CustomerSnapshot` from a customer event. Default returns
    /// `None`, which skips the email / metadata refresh on the existing row.
    fn extract_customer_snapshot(&self, event: &WebhookEvent) -> Option<CustomerSnapshot>;
}
```

In practice you never call any of these directly — `webhook_routes` invokes them for every inbound webhook. They live on the trait so adapter crates can implement provider-specific signature verification, event parsing, and payload extraction in a testable way. The `extract_*` methods all have sensible defaults; the shipped Stripe and Paddle adapters override them with provider-shape-aware implementations (Stripe reaches into `data.object.*`, Paddle into `data.*`).

## The flow-tagged Inertia payload

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
    /// Mobile Money flow — no redirect or embed. Frontend displays a
    /// user-facing message telling the customer to confirm on their phone
    /// (USSD prompt or operator app), then polls the provider via
    /// `provider_transaction_id` for status updates.
    MobileMoneyPrompt {
        provider_transaction_id: String,
        message: String,
        operator: MobileMoneyOperator,
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

A `MobileMoneyPrompt` payload looks like this — there is no URL because the customer never leaves your page; the frontend renders `message` and starts polling:

```json
{
  "flow": "mobile_money_prompt",
  "provider_transaction_id": "ch_mm_...",
  "message": "Check your phone for the MTN MoMo prompt.",
  "operator": { "kind": "mtn_momo" }
}
```

Return whichever variant the provider produces from your controller as Inertia props. Frontend integration is described in [Payments — Frontend Integration](payments-frontend.md).

## Mirror tables

Six tables are created by the framework migration. Pull in the public alias and include it in your app's migrator:

```rust,ignore
use sea_orm_migration::{MigrationTrait, MigratorTrait};
use suprnova::payments::migrations::CreatePaymentsTables;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            // ... your other migrations ...
            Box::new(CreatePaymentsTables),
        ]
    }
}
```

The same module also exports a helper `pub fn migrations() -> Vec<Box<dyn MigrationTrait>>` if you'd rather call that and spread the result into your own list.

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

## Webhook handling

Mount the webhook ingress route once at bootstrap — see the [Quick start](#quick-start) routes example for the composition pattern. `webhook_routes(db)` returns a `Router` carrying the single `POST /webhooks/payments/{provider}` handler that's built into the framework. You chain your own routes onto it (or call the route's underlying primitives directly inside your own `routes!{}` block).

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

### Mirror-table hydration

After the audit row is persisted, the framework dispatches the event to the relevant mirror table based on `neutral`. **All mirror writes for one event happen inside a single DB transaction along with `mark_processed`** — partial mirror state is never observable. Either everything commits together or everything rolls back.

| `NeutralEventKind`               | Mirror effect                                                                                       |
|----------------------------------|-----------------------------------------------------------------------------------------------------|
| `SubscriptionCreated/Updated`    | Calls `Subscription::get(id)` on the provider, upserts `payments_subscriptions`, syncs items.       |
| `SubscriptionCanceled`           | Same as above; also sets `canceled_at` and flips `status` to `canceled` on the existing row.        |
| `PaymentSucceeded / Failed / Refunded / Disputed` | Upserts `payments_transactions` from the snapshot the provider produces from `raw_payload`.        |
| `InvoicePaid / InvoiceFailed`    | Upserts `payments_transactions` with `provider_subscription_id` linked.                              |
| `CustomerCreated / CustomerUpdated` | Updates the existing `payments_customers` row's `email` / `provider_metadata` from the provider's `CustomerSnapshot`. **Never inserts.**   |
| `None` (unmapped)                | Audit row only — no mirror change.                                                                   |

The customer mirror is intentionally update-only on the webhook path. `user_id` is `NOT NULL` and only the app knows which user a provider customer belongs to (the link is created by your code right after `CustomerStore::create_customer`). Out-of-band customers — created in the Stripe dashboard, say — are logged but never synthesized into the mirror.

### Failure recovery contract

The handler treats provider retries as the recovery mechanism:

- **Hydration succeeds:** transaction commits, `processed_at` set, `process_error` cleared. Response: `200 ok`.
- **Hydration fails:** transaction rolls back (no partial mirror state), audit row keeps `processed_at = NULL` and `process_error` records the failure. Response: `503 hydration-failed` — the provider will retry with backoff.
- **Provider retries the failed event:** idempotency check sees the existing audit row but `processed_at IS NULL`, so hydration runs again. The retry replaces the stale `process_error` with the current attempt's outcome.
- **Provider retries a succeeded event:** idempotency check sees `processed_at IS NOT NULL`, returns `200 duplicate` immediately. No re-hydration.

A subscription/customer event with a missing `subscription_id` / `customer_id` in the payload is treated as a `Validation` error (also 503 + `process_error` recorded). Silent success on a malformed payload would leave the mirror stale without operator visibility.

Items removed from a subscription on the provider side (e.g. user dropped a seat add-on) are removed from `payments_subscription_items` when the next `subscription.updated` webhook arrives. The provider's `Subscription::get(id)` response is the source of truth on every sync.

## Payment methods beyond cards

`PaymentMethod` is the enum the framework uses for stored methods in `payments_payment_methods` and for any provider that exposes method metadata. It covers the obvious cases — cards, bank transfers, e-wallets — plus regional methods that are first-class in many markets:

```rust,ignore
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PaymentMethod {
    Card { brand: String, last4: String, exp_month: u8, exp_year: u16 },
    BankTransfer { bank_name: String, last4: String },
    EWallet { provider: String, identifier: String },
    /// Payer identified by phone + operator + country.
    MobileMoney {
        operator: MobileMoneyOperator,
        phone: PhoneNumber,
        country: CountryCode,
    },
    /// Pegged crypto — cash-equivalent for most providers.
    Stablecoin { asset: StablecoinAsset, network: Option<String> },
    /// Non-pegged cryptocurrency.
    Crypto { network: String, address: String },
    /// Escape hatch for regional / provider-specific methods not yet modeled.
    Custom { kind: String, descriptor: String },
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MobileMoneyOperator {
    MtnMomo,
    Mpesa,
    AirtelMoney,
    OrangeMoney,
    Lipila,
    Custom { identifier: String },
}

#[serde(tag = "kind", rename_all = "snake_case")]
pub enum StablecoinAsset {
    Usdc,
    Usdt,
    Dai,
    Custom { ticker: String },
}
```

The named operators and assets are the ones we've enumerated. The `Custom { ... }` variants on each cover regional operators and stablecoins we haven't pinned yet, so adding support for one doesn't force a framework release.

`PhoneNumber` and `CountryCode` are validated DTOs in `suprnova::payments` — they reject malformed input at construction time, which is where you want the failure rather than at the provider call.

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

Handle `RequiresClientAction` by returning the payload to your frontend. The frontend renders the 3DS challenge using `client_secret` + `publishable_key`. See [Payments — Frontend Integration](payments-frontend.md) for the frontend dispatch code.

## Idempotency keys

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

## The discriminator pattern

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

## Multi-provider apps

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

## Next

- [Payments — Stripe Adapter](payments-stripe.md) — the gateway flow in detail: PaymentIntents, webhook signature format, event-type mapping
- [Payments — Paddle Adapter](payments-paddle.md) — the MoR flow in detail: checkout-driven subscription creation, tax handling, notification verification
- [Payments — Frontend Integration](payments-frontend.md) — Svelte 5, React 19, and Vue 3.5 dispatch-on-flow examples
- [Writing a Payment Provider Adapter](payments-provider-guide.md) — build your own adapter crate end to end
- [Database](database.md) — the SeaORM layer the mirror tables sit on
