# Writing a Payment Provider Adapter

This guide walks through building a third-party adapter crate — `suprnova-payments-mollie` — that plugs into Suprnova's provider-neutral payments surface. By the end you will have a crate that registers itself, passes the discriminator flow, and can be dropped into any Suprnova app with a single `cargo add`.

The same structure applies to any provider: Square, Braintree, Adyen, or anything else with an HTTP API.

## 1. Create the Workspace Member Crate

From the repo root:

```bash
cargo new --lib crates/suprnova-payments-mollie
```

Add it to your root `Cargo.toml`:

```toml
[workspace]
members = [
    "framework",
    "suprnova-cli",
    "suprnova-macros",
    "app",
    "crates/suprnova-payments-mollie",  # add this line
]
```

**`crates/suprnova-payments-mollie/Cargo.toml`:**

```toml
[package]
name = "suprnova-payments-mollie"
version.workspace = true
edition.workspace = true
license.workspace = true
description = "Mollie payment adapter for Suprnova"

[dependencies]
suprnova = { path = "../../framework" }
async-trait = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
thiserror = "2"
inventory = "0.3"
tracing = "0.1"
tokio = { version = "1", features = ["macros", "rt"] }
# Your Mollie SDK:
mollie-rs = "0.1"
hmac = "0.12"   # for webhook HMAC verification
sha2 = "0.10"
hex = "0.4"

[dev-dependencies]
tokio = { version = "1", features = ["full"] }
```

## 2. Lay Out the Source Files

Mirror the structure used by the shipped adapters:

```
crates/suprnova-payments-mollie/src/
├── lib.rs          # MollieProvider struct, PaymentProvider impl, from_env
├── checkout.rs     # Checkout impl
├── customer.rs     # CustomerStore impl
├── subscription.rs # Subscription impl
├── webhook.rs      # WebhookHandler impl
├── event_map.rs    # provider event string → NeutralEventKind
└── payment.rs      # Payment impl (if Mollie supports server-capture)
```

## 3. `lib.rs` — the Provider Struct

```rust,ignore
use async_trait::async_trait;
use suprnova::payments::{Payment, PaymentProvider};

mod checkout;
mod customer;
mod event_map;
mod payment;
mod subscription;
mod webhook;

pub use event_map::mollie_event_to_neutral;

/// Mollie adapter for Suprnova's provider-neutral payments surface.
#[derive(Clone, Debug)]
pub struct MollieProvider {
    /// Mollie API key (`test_…` / `live_…`).
    api_key: String,
    /// Webhook signing secret — used in HMAC verification.
    webhook_secret: String,
    /// HTTP client — share across requests.
    client: reqwest::Client,
}

impl MollieProvider {
    pub fn new(api_key: impl Into<String>, webhook_secret: impl Into<String>) -> Self {
        Self {
            api_key: api_key.into(),
            webhook_secret: webhook_secret.into(),
            client: reqwest::Client::new(),
        }
    }

    /// Construct from environment variables.
    ///
    /// Reads:
    /// - `MOLLIE_API_KEY`
    /// - `MOLLIE_WEBHOOK_SECRET`
    pub fn from_env() -> Result<Self, String> {
        let api_key = std::env::var("MOLLIE_API_KEY")
            .map_err(|_| "MOLLIE_API_KEY not set".to_string())?;
        let webhook_secret = std::env::var("MOLLIE_WEBHOOK_SECRET")
            .map_err(|_| "MOLLIE_WEBHOOK_SECRET not set".to_string())?;
        Ok(Self::new(api_key, webhook_secret))
    }
}

impl PaymentProvider for MollieProvider {
    fn name(&self) -> &'static str {
        "mollie"
    }

    // If Mollie supports server-capture, override as_payment():
    fn as_payment(&self) -> Option<&dyn Payment> {
        Some(self)  // remove this line if not implementing Payment
    }
}
```

`PaymentProvider` is the umbrella trait. It requires `Checkout + Subscription + CustomerStore + WebhookHandler`. You implement each in a dedicated module.

## 4. Implement the Four Universal Traits

### `checkout.rs`

```rust,ignore
use async_trait::async_trait;
use suprnova::payments::{
    Checkout, PaymentError, PaymentResult, SessionMode, SessionPayload, StartSessionRequest,
};

use crate::MollieProvider;

#[async_trait]
impl Checkout for MollieProvider {
    async fn start_session(&self, req: StartSessionRequest) -> PaymentResult<SessionPayload> {
        // Call the Mollie API to create a payment or order.
        // Map the response to one of the SessionPayload variants.
        // Mollie uses hosted checkout pages, so Redirect is the natural fit.
        let checkout_url = self.create_mollie_payment(&req).await
            .map_err(|e| PaymentError::Internal(format!("Mollie checkout error: {e}")))?;

        Ok(SessionPayload::Redirect {
            url: checkout_url,
            provider_session_id: "mollie_session_id_here".into(),
        })
    }
}

impl MollieProvider {
    async fn create_mollie_payment(&self, req: &StartSessionRequest) -> Result<String, mollie_rs::Error> {
        // Wire up the Mollie SDK call here.
        // Return the hosted checkout URL.
        todo!("Mollie payment creation")
    }
}
```

### `customer.rs`

```rust,ignore
use async_trait::async_trait;
use suprnova::payments::{
    CreateCustomerRequest, CustomerRef, CustomerStore, PaymentError, PaymentResult,
    UpdateCustomerRequest,
};

use crate::MollieProvider;

#[async_trait]
impl CustomerStore for MollieProvider {
    async fn create_customer(&self, req: CreateCustomerRequest) -> PaymentResult<CustomerRef> {
        // POST /v2/customers to Mollie
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn update_customer(&self, req: UpdateCustomerRequest) -> PaymentResult<CustomerRef> {
        // PATCH /v2/customers/{id}
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn get_customer(&self, provider_customer_id: &str) -> PaymentResult<CustomerRef> {
        // GET /v2/customers/{id}
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn delete_customer(&self, provider_customer_id: &str) -> PaymentResult<()> {
        // DELETE /v2/customers/{id}
        Err(PaymentError::Internal("not yet implemented".into()))
    }
}
```

### `subscription.rs`

```rust,ignore
use async_trait::async_trait;
use suprnova::payments::{
    PaymentError, PaymentResult, SubscribeRequest, Subscription, SubscriptionResult,
    UpdateSubscriptionRequest,
};

use crate::MollieProvider;

#[async_trait]
impl Subscription for MollieProvider {
    async fn subscribe(&self, req: SubscribeRequest) -> PaymentResult<SubscriptionResult> {
        // POST /v2/customers/{id}/subscriptions
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn update(&self, req: UpdateSubscriptionRequest) -> PaymentResult<SubscriptionResult> {
        // PATCH /v2/customers/{id}/subscriptions/{sub_id}
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn cancel(
        &self,
        provider_subscription_id: &str,
        at_period_end: bool,
    ) -> PaymentResult<SubscriptionResult> {
        if at_period_end {
            // Set cancel date to period end
        } else {
            // DELETE /v2/customers/{id}/subscriptions/{sub_id}
        }
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn get(&self, provider_subscription_id: &str) -> PaymentResult<SubscriptionResult> {
        // GET /v2/customers/{id}/subscriptions/{sub_id}
        Err(PaymentError::Internal("not yet implemented".into()))
    }
}
```

If your provider doesn't support a method, return `PaymentError::NotSupported`:

```rust,ignore
Err(PaymentError::NotSupported(
    "Mollie creates subscriptions via checkout — use start_session instead".into()
))
```

### `payment.rs` — server-side capture (optional)

Only implement this if your provider supports direct server-side charges against a stored payment method. Remove the `as_payment()` override in `lib.rs` if you skip this.

```rust,ignore
use async_trait::async_trait;
use suprnova::payments::{
    ChargeRequest, ChargeResult, Payment, PaymentError, PaymentResult, PaymentStatus,
    RefundRequest, RefundResult,
};

use crate::MollieProvider;

#[async_trait]
impl Payment for MollieProvider {
    async fn charge(&self, req: ChargeRequest) -> PaymentResult<ChargeResult> {
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn capture(&self, provider_transaction_id: &str) -> PaymentResult<ChargeResult> {
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn refund(&self, req: RefundRequest) -> PaymentResult<RefundResult> {
        // POST /v2/payments/{id}/refunds
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn void(&self, provider_transaction_id: &str) -> PaymentResult<()> {
        Err(PaymentError::Internal("not yet implemented".into()))
    }

    async fn status(&self, provider_transaction_id: &str) -> PaymentResult<PaymentStatus> {
        Err(PaymentError::Internal("not yet implemented".into()))
    }
}
```

## 5. Map Provider Events to `NeutralEventKind`

**`event_map.rs`:**

```rust,ignore
use suprnova::payments::NeutralEventKind;

/// Map a Mollie webhook event type string to the framework's neutral taxonomy.
/// Returns `None` for provider-specific events that have no neutral equivalent.
pub fn mollie_event_to_neutral(event_type: &str) -> Option<NeutralEventKind> {
    match event_type {
        // Mollie payments
        "payment.paid"          => Some(NeutralEventKind::PaymentSucceeded),
        "payment.failed"        => Some(NeutralEventKind::PaymentFailed),
        "payment.expired"       => Some(NeutralEventKind::PaymentFailed),
        "refund.created"        => Some(NeutralEventKind::PaymentRefunded),
        "chargeback.created"    => Some(NeutralEventKind::PaymentDisputed),
        // Mollie subscriptions
        "subscription.created"  => Some(NeutralEventKind::SubscriptionCreated),
        "subscription.updated"  => Some(NeutralEventKind::SubscriptionUpdated),
        "subscription.canceled" => Some(NeutralEventKind::SubscriptionCanceled),
        // Mollie orders/invoices
        "order.paid"            => Some(NeutralEventKind::InvoicePaid),
        // Customer events
        "customer.created"      => Some(NeutralEventKind::CustomerCreated),
        "customer.updated"      => Some(NeutralEventKind::CustomerUpdated),
        // Provider-specific — falls through to raw_payload
        _                       => None,
    }
}
```

Cover at minimum the events listed above. For any event not in the neutral taxonomy, return `None` — it still gets persisted in `payments_webhook_events` under `provider_event_type` + `raw_payload` so domain code can read it.

## 6. Implement Webhook Signature Verification

**`webhook.rs`:**

Mollie signs webhook payloads using HMAC-SHA256. Always compare signatures in constant time to prevent timing attacks.

```rust,ignore
use async_trait::async_trait;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use suprnova::payments::{
    NeutralEventKind, PaymentError, PaymentResult, WebhookContext, WebhookEvent, WebhookHandler,
};

use crate::{MollieProvider, event_map::mollie_event_to_neutral};

type HmacSha256 = Hmac<Sha256>;

#[async_trait]
impl WebhookHandler for MollieProvider {
    fn verify(&self, ctx: &WebhookContext<'_>) -> PaymentResult<()> {
        // Read the signature header Mollie sends.
        // Exact header name and signing scheme — check Mollie's docs for your version.
        let signature = ctx
            .headers
            .get("X-Mollie-Signature")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(|| PaymentError::WebhookSignatureMissing)?;

        // Compute expected HMAC-SHA256 over the raw body.
        let mut mac = HmacSha256::new_from_slice(self.webhook_secret.as_bytes())
            .map_err(|e| PaymentError::Internal(format!("HMAC init: {e}")))?;
        mac.update(ctx.body);

        // Decode the hex-encoded received signature.
        let received = hex::decode(signature)
            .map_err(|_| PaymentError::WebhookSignatureInvalid)?;

        // Constant-time comparison.
        mac.verify_slice(&received)
            .map_err(|_| PaymentError::WebhookSignatureInvalid)
    }

    fn parse_event(&self, body: &[u8]) -> PaymentResult<WebhookEvent> {
        // Mollie sends JSON — parse it.
        let raw: serde_json::Value = serde_json::from_slice(body)
            .map_err(|e| PaymentError::WebhookParseError(format!("JSON parse: {e}")))?;

        let event_id = raw["id"].as_str()
            .ok_or_else(|| PaymentError::WebhookParseError("missing event id".into()))?
            .to_string();

        // Mollie uses resource types rather than event type strings in some webhook shapes.
        // Adapt to whatever your SDK version sends.
        let event_type = raw["resource"].as_str()
            .unwrap_or("unknown")
            .to_string();

        let neutral = mollie_event_to_neutral(&event_type);

        Ok(WebhookEvent {
            provider: "mollie".into(),
            provider_event_id: event_id,
            provider_event_type: event_type,
            neutral,
            raw_payload: raw,
        })
    }
}
```

Key points:

- `WebhookSignatureMissing` and `WebhookSignatureInvalid` are the correct `PaymentError` variants for missing and bad signatures.
- The framework's `webhook_routes` handler calls `verify` before `parse_event` and returns 401 on verification failure, 400 on parse failure.
- Never log the raw secret or the received signature.

### Mirror-table hydration: `extract_payload_ids` + `extract_payment_snapshot` + `extract_customer_snapshot`

After `parse_event` returns a `WebhookEvent`, the framework's webhook route hydrates the mirror tables. Three optional trait methods drive that — all have safe default no-op implementations, so an adapter can ship without them and still pass through the audit layer:

```rust,ignore
fn extract_payload_ids(&self, event: &WebhookEvent) -> PayloadIds;
fn extract_payment_snapshot(&self, event: &WebhookEvent) -> Option<PaymentSnapshot>;
fn extract_customer_snapshot(&self, event: &WebhookEvent) -> Option<CustomerSnapshot>;
```

`PayloadIds` is the bridge between the parsed event and the framework's mirror logic. Implement it so the framework can find the right entity:

```rust,ignore
pub struct PayloadIds {
    pub subscription_id: Option<String>,
    pub customer_id: Option<String>,
    pub transaction_id: Option<String>,
}
```

For each `neutral` value, populate the IDs that the provider's payload exposes. Subscription events should set `subscription_id` so the framework can call `Subscription::get(id)` and refresh the mirror from the canonical state. Customer events set `customer_id`. Payment / invoice events set `transaction_id`, plus `subscription_id` when it's a recurring charge.

`PaymentSnapshot` is built directly from the webhook payload — there's no `Payment::get` callback. Implement it for payment / invoice neutrals:

```rust,ignore
pub struct PaymentSnapshot {
    pub provider_transaction_id: String,
    pub provider_customer_id: String,
    pub provider_subscription_id: Option<String>,
    pub amount_total_minor: i64,
    pub amount_tax_minor: i64,
    pub currency: String,
    pub status: String,             // "succeeded" | "failed" | "refunded" | "disputed"
    pub paid_at: Option<DateTime<Utc>>,
    pub provider_metadata: Value,   // typically the entity object from the payload
}
```

Stripe's reference implementation reads `data.object.{id,amount,currency,customer}` for `PaymentIntent`/`Charge` events and `data.object.{id,amount_paid,tax,currency,customer,subscription,status_transitions.paid_at}` for `Invoice` events. Paddle's reads `data.{id,customer_id,currency_code,details.totals.{total,tax},billed_at,subscription_id}`. Mirror the conventions that match your provider's payload shape — the framework doesn't care how you extract, only that the snapshot is correct.

If you return `None` from `extract_payment_snapshot`, the audit row is still written but `payments_transactions` is not touched. That is the correct return for subscription / customer events, or for any payment event where the payload doesn't carry enough information to populate a row.

`CustomerSnapshot` keeps customer-mirror sync provider-driven (no hardcoded JSON paths in the framework):

```rust,ignore
pub struct CustomerSnapshot {
    pub provider_customer_id: String,
    pub email: Option<String>,
    pub provider_metadata: Value,
}
```

The framework will `email = Set(snapshot.email)` only when the snapshot supplies one; `provider_metadata` is always replaced with the provider's view of the customer (`updated_at` is also bumped regardless). Customer-mirror rows are only ever **updated** — never inserted — because `user_id` is `NOT NULL` and the app owns the user ↔ customer link via `CustomerStore::create_customer`.

### Failure semantics

If `extract_payload_ids` returns `None` for `subscription_id` on a subscription event (or for `customer_id` on a customer event), the framework treats that as a `Validation` error: the hydration transaction rolls back, the audit row's `process_error` is set, and the HTTP response is **503 hydration-failed** so the provider retries. Silent success on a malformed payload would leave the mirror stale without operator visibility — provider retries are the recovery mechanism.

This contract means an adapter's extractor must populate the relevant IDs honestly. Returning `None` is reserved for events your provider can't translate at all (e.g. a payment event with no charge ID in the payload), not for "I didn't bother to parse this one."

## 7. Register at App Boot

Two mechanisms are available — pick one:

### Runtime registration (recommended for apps with env-var config)

```rust,ignore
use std::sync::Arc;
use suprnova::payments::PaymentProviderRegistry;
use suprnova_payments_mollie::MollieProvider;

let mollie = MollieProvider::from_env().expect("Mollie env vars not set");
PaymentProviderRegistry::bind("mollie", Arc::new(mollie));
```

### Compile-time registration via `inventory`

For adapter crates that want zero-config registration — useful when shipping a library that consumers just `cargo add` without any boot-time wiring:

```rust,ignore
use suprnova::payments::{PaymentProviderEntry, PaymentProviderRegistry};
use inventory;

// In lib.rs, in a static initializer:
inventory::submit!(PaymentProviderEntry {
    name: "mollie",
    factory: || Arc::new(MollieProvider::from_env().expect("Mollie env not set")),
});
```

`inventory::submit!` runs before `main`. The factory closure is called once when the registry is first accessed.

## 8. Pass the Discriminator Test

Every adapter crate should include an integration test that proves the trait contract is correct end to end. This is the soundness proof — if this test passes, the provider plugs into any Suprnova app without surprises.

```rust,ignore
// tests/discriminator.rs (inside crates/suprnova-payments-mollie/)

use suprnova::payments::*;
use suprnova_payments_mollie::MollieProvider;

/// Requires MOLLIE_API_KEY and MOLLIE_WEBHOOK_SECRET to be set.
/// Run with: cargo test --test discriminator -- --ignored
#[tokio::test]
#[ignore = "requires live Mollie sandbox credentials"]
async fn discriminator_flow() {
    let provider = MollieProvider::from_env().expect("Mollie env vars not set");

    // 1. Create customer
    let cus = provider.create_customer(CreateCustomerRequest {
        user_id: "test_user_1".into(),
        email: "test@example.com".into(),
        name: Some("Test User".into()),
        metadata: None,
    }).await.expect("create_customer failed");
    assert!(!cus.provider_customer_id.is_empty());

    // 2. Start checkout session
    let session = provider.start_session(StartSessionRequest {
        mode: SessionMode::Subscription,
        customer_ref: cus.provider_customer_id.clone(),
        price_refs: vec!["your_mollie_plan_id".into()],
        success_return_url: "https://app.example/billing/success".into(),
        cancel_return_url: "https://app.example/billing/cancel".into(),
        amount_hint: None,
        idempotency_key: Some("discriminator_test_checkout".into()),
        metadata: None,
    }).await.expect("start_session failed");
    assert!(matches!(session, SessionPayload::Redirect { .. }));

    // 3. Subscribe directly (if your provider supports it; Mollie may require checkout)
    let sub = provider.subscribe(SubscribeRequest {
        customer_ref: cus.provider_customer_id.clone(),
        price_refs: vec!["your_mollie_plan_id".into()],
        trial_days: None,
        idempotency_key: Some("discriminator_test_sub".into()),
        metadata: None,
    }).await.expect("subscribe failed");
    assert_eq!(sub.status, SubscriptionStatus::Active);

    // 4. Read back
    let fetched = provider.get(&sub.provider_subscription_id).await.expect("get failed");
    assert_eq!(fetched.provider_subscription_id, sub.provider_subscription_id);

    // 5. Cancel at period end
    let s = provider.cancel(&sub.provider_subscription_id, true).await.expect("cancel failed");
    assert!(s.cancel_at_period_end);

    // 6. Cancel immediately
    let s = provider.cancel(&sub.provider_subscription_id, false).await.expect("cancel failed");
    assert_eq!(s.status, SubscriptionStatus::Canceled);

    // 7. Verify as_payment() invariant
    let p: &dyn PaymentProvider = &provider;
    // If you implemented Payment: assert!(p.as_payment().is_some())
    // If you did NOT implement Payment: assert!(p.as_payment().is_none())
    let _ = p.as_payment();
}
```

Gate live integration tests with `#[ignore]` so `cargo test` passes in CI without credentials. Run them explicitly with `-- --ignored` against a sandbox account.

## 9. `PaymentError` Variants Reference

Use the correct variants when returning errors:

| Variant | When to use |
|---|---|
| `PaymentError::Internal(String)` | Unexpected SDK error, network failure, or unimplemented method |
| `PaymentError::NotFound(String)` | Customer, subscription, or transaction ID doesn't exist |
| `PaymentError::NotSupported(String)` | The method isn't applicable for this provider (e.g. Paddle's `subscribe`) |
| `PaymentError::WebhookSignatureMissing` | Required signature header absent |
| `PaymentError::WebhookSignatureInvalid` | Signature present but does not verify |
| `PaymentError::WebhookParseError(String)` | Body is not parseable as the expected format |

## What's Next

- Add your crate to your app's `Cargo.toml` with `cargo add suprnova-payments-mollie --path ./crates/suprnova-payments-mollie`
- Register at bootstrap as shown in step 7
- Mount `webhook_routes(db.clone())` if you haven't already — it handles all registered providers automatically
- See [`payments-frontend.md`](./payments-frontend.md) for how to render the `SessionPayload` your adapter returns
