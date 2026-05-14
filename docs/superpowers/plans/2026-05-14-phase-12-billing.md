# Phase 12: Billing (Cashier-style on PayRail) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Laravel-Cashier-shape subscription billing — `user.subscribe("pro", "price_xxx").await?`, `user.subscription("pro").cancel().await?`, `Invoice::for_subscription(...).render_pdf()`, customer-portal redirects, webhook → typed event fanout. Built on top of [`payrail`](https://github.com/boniface/payrail) (MIT/Apache-2.0) for the payment-rails layer, with Suprnova owning the subscription / invoice business-logic layer on top.

**Architecture — two distinct layers:**

| Layer | Source | Responsibility |
|---|---|---|
| **Payment rails** | **PayRail** | One-time charges, refunds, captures, webhook signature verification + normalized `PaymentEvent`, provider routing across Stripe / PayPal / Lipila / MTN MoMo / M-Pesa / Airtel Money / Flutterwave / Paystack / Orange Money / Circle / Coinbase / Bridge / Binance |
| **Subscription business logic** | **Ours** | `Subscription` model + lifecycle (trialing → active → past_due → cancelled), `Invoice` + PDF rendering, period-end charging job, `Billable` user trait, customer-portal redirects, typed event fanout (`SubscriptionCreated` / `InvoicePaid` / `SubscriptionCancelled` / `TrialEnding`) via Phase 1 `EventDispatcher` |

The bridge: a single `WebhookHandler` per provider receives the raw payload, asks PayRail to verify the signature + normalize into `PaymentEvent`, then our subscription state machine reacts (mark invoice paid, transition subscription to past_due, etc.) and dispatches typed framework events. App listeners hook those via `Event::listen::<InvoicePaid>(...)`.

**Why PayRail over Hyperswitch:** PayRail is a library; Hyperswitch is a deployment. PayRail's 13 builtin providers cover the markets Laravel Cashier doesn't reach — Mobile Money for African markets (Lipila, MTN MoMo, M-Pesa, Airtel, Orange) and crypto rails (Circle, Coinbase, Bridge, Binance) — without our consumers running a separate payments-router service.

**Paddle note:** Paddle is not in PayRail's `BuiltinProvider` enum. v1 of Phase 12 ships without Paddle; if a consumer needs it, the path is (a) fork PayRail into workspace as `suprnova-payrail` and add a `Paddle` variant, or (b) contribute upstream. Documented under "Out of v1 scope" below.

**Tech Stack:** `payrail` (`path = "../reference/payrail-0.1.5/crates/payrail"`, feature `all-providers`), `printpdf` 0.7 for invoice PDFs, reuses Phase 1 events, Phase 5 mail, Phase 5 queue (for period-end charging job), Phase 2 HTTP (for billing-portal URL retrieval where the provider needs it).

---

## File Structure

**New files:**
- `framework/src/billing/mod.rs` — `Billable` trait, `Subscription` model, facade
- `framework/src/billing/subscription.rs` — lifecycle state machine
- `framework/src/billing/invoice.rs` — `Invoice` struct + PDF rendering
- `framework/src/billing/period_job.rs` — `ChargeSubscriptionJob` (period-end charging)
- `framework/src/billing/webhook.rs` — per-provider webhook handlers, PayRail-bridge
- `framework/src/billing/events.rs` — typed framework events
- `framework/src/billing/portal.rs` — customer-portal URL builders (provider-specific)
- `framework/src/billing/migrations/m_create_subscriptions_table.rs`
- `framework/src/billing/migrations/m_create_invoices_table.rs`
- `framework/src/billing/migrations/m_add_billing_columns_to_users.rs`
- `framework/tests/billing.rs` — end-to-end with PayRail mock provider
- `app/src/listeners/billing_listener.rs` — dogfood

**Modified files:**
- `framework/Cargo.toml` — add `payrail = { path = "../reference/payrail-0.1.5/crates/payrail", features = ["all-providers"] }`, `printpdf`
- `framework/src/lib.rs` — re-export `Billable`, `Subscription`, `Invoice`

---

## Task 1: Add deps + migrations

**Files:** `framework/Cargo.toml`, migrations

- [ ] **Step 1: Add deps**

```toml
# framework/Cargo.toml — [dependencies]
payrail = { path = "../reference/payrail-0.1.5/crates/payrail", features = ["all-providers"] }
printpdf = "0.7"
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Migration schemas**

```rust
// framework/src/billing/migrations/m_create_subscriptions_table.rs
// CREATE TABLE subscriptions (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   user_id BIGINT NOT NULL,
//   name VARCHAR(64) NOT NULL,             -- "default" | "pro" | "addons" | ...
//   provider VARCHAR(32) NOT NULL,          -- "Stripe" | "PayPal" | "Lipila" | ...
//   provider_subscription_id VARCHAR(128),  -- PayRail ProviderReference for the subscription, when the rails offer one
//   price_id VARCHAR(128) NOT NULL,
//   status VARCHAR(32) NOT NULL,            -- "trialing" | "active" | "past_due" | "cancelled" | "incomplete"
//   trial_ends_at DATETIME,
//   current_period_start DATETIME NOT NULL,
//   current_period_end DATETIME NOT NULL,
//   cancel_at_period_end BOOLEAN NOT NULL DEFAULT FALSE,
//   ends_at DATETIME,
//   quantity INTEGER NOT NULL DEFAULT 1,
//   metadata JSON,
//   created_at DATETIME NOT NULL,
//   updated_at DATETIME NOT NULL,
//   INDEX (user_id, status),
//   INDEX (current_period_end, status)
// );
```

```rust
// framework/src/billing/migrations/m_create_invoices_table.rs
// CREATE TABLE invoices (
//   id BIGINT PRIMARY KEY AUTO_INCREMENT,
//   subscription_id BIGINT,                 -- NULL for one-off invoices
//   user_id BIGINT NOT NULL,
//   number VARCHAR(64) NOT NULL UNIQUE,     -- "INV-2026-000123"
//   provider VARCHAR(32) NOT NULL,
//   provider_invoice_id VARCHAR(128),       -- PayRail PaymentId where applicable
//   amount_cents BIGINT NOT NULL,
//   currency VARCHAR(3) NOT NULL,
//   status VARCHAR(32) NOT NULL,            -- "open" | "paid" | "void" | "uncollectible"
//   period_start DATETIME,
//   period_end DATETIME,
//   paid_at DATETIME,
//   line_items JSON NOT NULL,
//   created_at DATETIME NOT NULL,
//   INDEX (user_id, status),
//   INDEX (subscription_id)
// );
```

```rust
// framework/src/billing/migrations/m_add_billing_columns_to_users.rs
// ALTER TABLE users
//   ADD COLUMN stripe_customer_id VARCHAR(128),
//   ADD COLUMN paypal_customer_id VARCHAR(128),
//   ADD COLUMN lipila_customer_id VARCHAR(128),
//   ADD COLUMN preferred_payment_provider VARCHAR(32),
//   ADD COLUMN trial_ends_at DATETIME;
```

- [ ] **Step 4: Commit**

```bash
git add framework/Cargo.toml Cargo.lock framework/src/billing/migrations
git commit -m "feat(billing): add payrail + printpdf deps + subscription/invoice migrations"
```

---

## Task 2: PayRail client + Subscription / Billable foundations

**Files:** `framework/src/billing/mod.rs`, `framework/src/billing/subscription.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/billing.rs
use suprnova::{Billable, Subscription};

#[derive(Debug, Clone)]
struct User {
    id: i64,
    email: String,
    stripe_customer_id: Option<String>,
}

#[suprnova::async_trait]
impl Billable for User {
    fn user_id(&self) -> i64 { self.id }
    fn email(&self) -> &str { &self.email }

    async fn provider_customer_id(&self, provider: &payrail::PaymentProvider) -> Option<String> {
        match provider {
            payrail::PaymentProvider::Stripe => self.stripe_customer_id.clone(),
            _ => None,
        }
    }

    async fn set_provider_customer_id(
        &mut self,
        provider: &payrail::PaymentProvider,
        id: String,
    ) -> Result<(), suprnova::FrameworkError> {
        if matches!(provider, payrail::PaymentProvider::Stripe) {
            self.stripe_customer_id = Some(id);
        }
        Ok(())
    }
}

#[tokio::test]
async fn subscription_starts_in_trialing_when_trial_period_supplied() {
    let user = User { id: 1, email: "alice@example.com".into(), stripe_customer_id: None };
    let sub = Subscription::new_trialing(
        user.id,
        "pro",
        payrail::PaymentProvider::Stripe,
        "price_pro_monthly",
        chrono::Duration::days(14),
    );
    assert_eq!(sub.status, "trialing");
    assert!(sub.trial_ends_at.is_some());
    assert!(sub.current_period_end > chrono::Utc::now());
}

#[tokio::test]
async fn subscription_status_transitions_match_state_machine() {
    let mut sub = make_active_sub();
    sub.mark_past_due();
    assert_eq!(sub.status, "past_due");
    sub.mark_cancelled_at_period_end();
    assert_eq!(sub.status, "active"); // still active until period end
    assert!(sub.cancel_at_period_end);
    sub.mark_ended();
    assert_eq!(sub.status, "cancelled");
}

fn make_active_sub() -> Subscription {
    Subscription {
        id: 1,
        user_id: 1,
        name: "pro".into(),
        provider: payrail::PaymentProvider::Stripe,
        provider_subscription_id: Some("sub_xxx".into()),
        price_id: "price_pro_monthly".into(),
        status: "active".into(),
        trial_ends_at: None,
        current_period_start: chrono::Utc::now() - chrono::Duration::days(15),
        current_period_end: chrono::Utc::now() + chrono::Duration::days(15),
        cancel_at_period_end: false,
        ends_at: None,
        quantity: 1,
        metadata: serde_json::json!({}),
    }
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/billing/mod.rs
//! Subscription billing facade. Layered on top of `payrail` for the
//! payment-rails (one-time charges, webhooks, multi-provider
//! routing); we own the subscription lifecycle + invoices.

pub mod events;
pub mod invoice;
pub mod migrations;
pub mod period_job;
pub mod portal;
pub mod subscription;
pub mod webhook;

pub use events::{InvoicePaid, SubscriptionCancelled, SubscriptionCreated, TrialEnding};
pub use invoice::{Invoice, LineItem};
pub use subscription::Subscription;

use crate::FrameworkError;
use async_trait::async_trait;
use payrail::PaymentProvider;
use std::sync::OnceLock;

/// The configured PayRail client. Initialized once in bootstrap.
static PAYRAIL: OnceLock<payrail::PayRailClient> = OnceLock::new();

pub fn set_payrail(client: payrail::PayRailClient) {
    let _ = PAYRAIL.set(client);
}

pub(crate) fn payrail() -> Result<&'static payrail::PayRailClient, FrameworkError> {
    PAYRAIL
        .get()
        .ok_or_else(|| FrameworkError::internal("payrail client not initialized — call billing::set_payrail in bootstrap"))
}

/// Trait implemented on user models for billing operations.
#[async_trait]
pub trait Billable: Send + Sync {
    fn user_id(&self) -> i64;
    fn email(&self) -> &str;

    /// Provider-specific customer id from the user row.
    async fn provider_customer_id(&self, provider: &PaymentProvider) -> Option<String>;

    /// Persist a provider customer id back to the user row.
    async fn set_provider_customer_id(
        &mut self,
        provider: &PaymentProvider,
        id: String,
    ) -> Result<(), FrameworkError>;

    /// Subscribe to a plan. Returns the new Subscription row.
    async fn subscribe(
        &mut self,
        name: &str,
        provider: PaymentProvider,
        price_id: &str,
    ) -> Result<Subscription, FrameworkError> {
        subscription::create(self, name, provider, price_id, None).await
    }

    async fn subscribe_with_trial(
        &mut self,
        name: &str,
        provider: PaymentProvider,
        price_id: &str,
        trial_days: u32,
    ) -> Result<Subscription, FrameworkError> {
        subscription::create(self, name, provider, price_id, Some(trial_days)).await
    }

    /// Reference an existing subscription by name.
    fn subscription<'a>(&'a self, name: &'a str) -> SubscriptionRef<'a> {
        SubscriptionRef { user_id: self.user_id(), name }
    }

    /// One-off charge through PayRail.
    async fn invoice_for(
        &self,
        provider: PaymentProvider,
        amount_cents: i64,
        currency: &str,
        description: &str,
    ) -> Result<Invoice, FrameworkError> {
        invoice::create_one_off(self, provider, amount_cents, currency, description).await
    }

    /// URL to the provider's customer billing portal.
    async fn billing_portal_url(
        &self,
        provider: PaymentProvider,
        return_url: &str,
    ) -> Result<String, FrameworkError> {
        portal::url_for(self, provider, return_url).await
    }
}

pub struct SubscriptionRef<'a> {
    user_id: i64,
    name: &'a str,
}

impl<'a> SubscriptionRef<'a> {
    pub async fn active(&self) -> Result<bool, FrameworkError> {
        subscription::is_active(self.user_id, self.name).await
    }

    pub async fn on_trial(&self) -> Result<bool, FrameworkError> {
        subscription::on_trial(self.user_id, self.name).await
    }

    pub async fn cancel(&self) -> Result<(), FrameworkError> {
        subscription::cancel_at_period_end(self.user_id, self.name).await
    }

    pub async fn cancel_now(&self) -> Result<(), FrameworkError> {
        subscription::cancel_immediately(self.user_id, self.name).await
    }

    pub async fn resume(&self) -> Result<(), FrameworkError> {
        subscription::resume(self.user_id, self.name).await
    }

    pub async fn swap(&self, new_price_id: &str) -> Result<(), FrameworkError> {
        subscription::swap(self.user_id, self.name, new_price_id).await
    }
}
```

```rust
// framework/src/billing/subscription.rs
//! Subscription model + lifecycle state machine. The model persists
//! to the `subscriptions` table; the state machine is just typed
//! method calls that update `status` and related columns.

use crate::{Billable, FrameworkError};
use chrono::{DateTime, Duration, Utc};
use payrail::PaymentProvider;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    pub provider: PaymentProvider,
    pub provider_subscription_id: Option<String>,
    pub price_id: String,
    pub status: String,
    pub trial_ends_at: Option<DateTime<Utc>>,
    pub current_period_start: DateTime<Utc>,
    pub current_period_end: DateTime<Utc>,
    pub cancel_at_period_end: bool,
    pub ends_at: Option<DateTime<Utc>>,
    pub quantity: i32,
    pub metadata: serde_json::Value,
}

impl Subscription {
    pub fn new_trialing(
        user_id: i64,
        name: &str,
        provider: PaymentProvider,
        price_id: &str,
        trial_length: Duration,
    ) -> Self {
        let now = Utc::now();
        let trial_end = now + trial_length;
        Self {
            id: 0,
            user_id,
            name: name.to_string(),
            provider,
            provider_subscription_id: None,
            price_id: price_id.to_string(),
            status: "trialing".into(),
            trial_ends_at: Some(trial_end),
            current_period_start: now,
            current_period_end: trial_end,
            cancel_at_period_end: false,
            ends_at: None,
            quantity: 1,
            metadata: serde_json::json!({}),
        }
    }

    pub fn mark_past_due(&mut self) {
        self.status = "past_due".into();
    }

    pub fn mark_active(&mut self) {
        self.status = "active".into();
    }

    pub fn mark_cancelled_at_period_end(&mut self) {
        self.cancel_at_period_end = true;
    }

    pub fn mark_ended(&mut self) {
        self.status = "cancelled".into();
        self.ends_at = Some(Utc::now());
    }

    pub fn is_on_trial(&self) -> bool {
        self.status == "trialing" && self.trial_ends_at.map_or(false, |t| t > Utc::now())
    }

    pub fn is_active(&self) -> bool {
        matches!(self.status.as_str(), "trialing" | "active")
    }
}

/// Create a new subscription via PayRail.
pub(crate) async fn create<B: Billable + ?Sized>(
    user: &mut B,
    name: &str,
    provider: PaymentProvider,
    price_id: &str,
    trial_days: Option<u32>,
) -> Result<Subscription, FrameworkError> {
    // 1. Get or create the customer on the provider via PayRail.
    let customer_id = ensure_customer(user, &provider).await?;
    let _ = customer_id; // used by the actual provider charge below

    // 2. Construct + persist the subscription row.
    let mut sub = match trial_days {
        Some(days) => Subscription::new_trialing(
            user.user_id(),
            name,
            provider.clone(),
            price_id,
            Duration::days(days as i64),
        ),
        None => {
            // Active immediately; charge the first period via PayRail.
            let now = Utc::now();
            Subscription {
                id: 0,
                user_id: user.user_id(),
                name: name.to_string(),
                provider: provider.clone(),
                provider_subscription_id: None,
                price_id: price_id.to_string(),
                status: "active".into(),
                trial_ends_at: None,
                current_period_start: now,
                current_period_end: now + Duration::days(30), // default monthly; real impl reads plan
                cancel_at_period_end: false,
                ends_at: None,
                quantity: 1,
                metadata: serde_json::json!({}),
            }
        }
    };

    // INSERT INTO subscriptions ... RETURNING id;
    // (Sketch — implementer wires SeaORM ActiveModel insert.)
    sub.id = persist_new(&sub).await?;

    // 3. If not on trial, charge the first period via PayRail.
    if !sub.is_on_trial() {
        // Use payrail::CreatePaymentRequest, including idempotency key
        // derived from (user_id, subscription_id, period_start).
        // Sketch — concrete CreatePaymentRequest construction depends
        // on what `payrail` expects per provider; read
        // reference/payrail-0.1.5/crates/payrail/src/core/payment.rs
        // for the builder.
    }

    let _ = crate::Event::dispatch(crate::billing::events::SubscriptionCreated {
        user_id: sub.user_id,
        subscription_id: sub.id,
        plan: sub.price_id.clone(),
        provider: format!("{:?}", sub.provider),
    })
    .await;

    Ok(sub)
}

pub(crate) async fn is_active(user_id: i64, name: &str) -> Result<bool, FrameworkError> {
    let sub = find_by_user_name(user_id, name).await?;
    Ok(sub.map_or(false, |s| s.is_active()))
}

pub(crate) async fn on_trial(user_id: i64, name: &str) -> Result<bool, FrameworkError> {
    let sub = find_by_user_name(user_id, name).await?;
    Ok(sub.map_or(false, |s| s.is_on_trial()))
}

pub(crate) async fn cancel_at_period_end(user_id: i64, name: &str) -> Result<(), FrameworkError> {
    let mut sub = require(user_id, name).await?;
    sub.mark_cancelled_at_period_end();
    update(&sub).await?;
    let _ = crate::Event::dispatch(crate::billing::events::SubscriptionCancelled {
        user_id,
        subscription_name: name.to_string(),
        immediate: false,
    })
    .await;
    Ok(())
}

pub(crate) async fn cancel_immediately(user_id: i64, name: &str) -> Result<(), FrameworkError> {
    let mut sub = require(user_id, name).await?;
    sub.mark_ended();
    update(&sub).await?;
    let _ = crate::Event::dispatch(crate::billing::events::SubscriptionCancelled {
        user_id,
        subscription_name: name.to_string(),
        immediate: true,
    })
    .await;
    Ok(())
}

pub(crate) async fn resume(user_id: i64, name: &str) -> Result<(), FrameworkError> {
    let mut sub = require(user_id, name).await?;
    sub.cancel_at_period_end = false;
    update(&sub).await
}

pub(crate) async fn swap(user_id: i64, name: &str, new_price_id: &str) -> Result<(), FrameworkError> {
    let mut sub = require(user_id, name).await?;
    sub.price_id = new_price_id.to_string();
    update(&sub).await
}

async fn ensure_customer<B: Billable + ?Sized>(
    user: &mut B,
    provider: &PaymentProvider,
) -> Result<String, FrameworkError> {
    if let Some(id) = user.provider_customer_id(provider).await {
        return Ok(id);
    }
    // Create on the provider via PayRail. Sketch — PayRail's customer
    // creation surface; verify against payrail's `Customer` type and
    // the per-provider creation method on `PayRailClient`.
    let id = format!("cust_{}_{}", user.user_id(), uuid::Uuid::new_v4());
    user.set_provider_customer_id(provider, id.clone()).await?;
    Ok(id)
}

// === Persistence stubs — implementer wires SeaORM ActiveModel ===
async fn persist_new(_sub: &Subscription) -> Result<i64, FrameworkError> {
    unimplemented!("INSERT INTO subscriptions RETURNING id via SeaORM ActiveModel")
}
async fn update(_sub: &Subscription) -> Result<(), FrameworkError> {
    unimplemented!("UPDATE subscriptions ... via SeaORM ActiveModel")
}
async fn find_by_user_name(_user_id: i64, _name: &str) -> Result<Option<Subscription>, FrameworkError> {
    unimplemented!("SELECT FROM subscriptions WHERE user_id = ? AND name = ?")
}
async fn require(user_id: i64, name: &str) -> Result<Subscription, FrameworkError> {
    find_by_user_name(user_id, name)
        .await?
        .ok_or_else(|| FrameworkError::model_not_found("Subscription"))
}
```

```rust
// framework/src/lib.rs
pub mod billing;
pub use billing::{Billable, Invoice, Subscription};
```

> **PayRail builder + CreatePaymentRequest:** Verify the exact `PayRailBuilder` / `PayRailClient` / `CreatePaymentRequest` shapes via `reference/payrail-0.1.5/crates/payrail/src/{builder,client,core/payment}.rs`. The sketch above describes the architecture; field-by-field wiring is implementer work.

- [ ] **Step 3: Run + commit**

```bash
cargo test -p suprnova --test billing
git add framework/src/billing framework/src/lib.rs framework/tests/billing.rs
git commit -m "feat(billing): Billable trait + Subscription model + lifecycle state machine"
```

---

## Task 3: Bootstrap PayRail client

**Files:** `app/src/bootstrap.rs`, `framework/src/billing/mod.rs`

- [ ] **Step 1: Bootstrap helper**

```rust
// framework/src/billing/mod.rs — append
use payrail::PayRailBuilder;

/// Convenience builder for the standard "Stripe + PayPal + Lipila"
/// configuration. Apps with more exotic provider needs build the
/// PayRailClient directly and pass it to `set_payrail`.
pub async fn use_standard() -> Result<(), FrameworkError> {
    let mut builder = PayRailBuilder::default();

    // Each provider check is feature-gated against payrail's feature
    // flags + env vars present.
    #[cfg(feature = "billing-stripe")]
    if let Ok(key) = std::env::var("STRIPE_SECRET_KEY") {
        let webhook_secret = std::env::var("STRIPE_WEBHOOK_SECRET").ok();
        builder = builder.with_stripe(payrail::StripeConfig::new(
            secrecy::SecretString::from(key),
            webhook_secret.map(secrecy::SecretString::from),
        ));
    }
    #[cfg(feature = "billing-paypal")]
    if let (Ok(client_id), Ok(secret)) = (
        std::env::var("PAYPAL_CLIENT_ID"),
        std::env::var("PAYPAL_SECRET"),
    ) {
        builder = builder.with_paypal(payrail::PayPalConfig::new(client_id, secrecy::SecretString::from(secret)));
    }
    #[cfg(feature = "billing-lipila")]
    if let Ok(key) = std::env::var("LIPILA_API_KEY") {
        builder = builder.with_lipila(payrail::LipilaConfig::new(secrecy::SecretString::from(key)));
    }

    let client = builder
        .build()
        .map_err(|e| FrameworkError::internal(format!("payrail build: {}", e)))?;
    set_payrail(client);
    Ok(())
}
```

> **PayRail config types:** The exact `StripeConfig` / `PayPalConfig` / `LipilaConfig` constructor signatures live in `reference/payrail-0.1.5/crates/payrail/src/providers/<provider>/`. Use them; the sketch above is illustrative.

- [ ] **Step 2: Wire from app bootstrap**

```rust
// app/src/bootstrap.rs — inside register()
suprnova::billing::use_standard()
    .await
    .expect("payrail bootstrap");
```

- [ ] **Step 3: Cargo features**

```toml
# framework/Cargo.toml — [features]
billing-stripe = ["payrail/stripe"]
billing-paypal = ["payrail/paypal"]
billing-lipila = ["payrail/lipila"]
billing-mobile-money = ["payrail/mobile-money"]
billing-all = ["billing-stripe", "billing-paypal", "billing-lipila", "billing-mobile-money"]
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/billing/mod.rs framework/Cargo.toml app/src/bootstrap.rs
git commit -m "feat(billing): use_standard PayRail bootstrap + per-provider feature gates"
```

---

## Task 4: Webhook handler — PayRail bridge → typed events

**Files:** `framework/src/billing/webhook.rs`, `framework/src/billing/events.rs`

- [ ] **Step 1: Events**

```rust
// framework/src/billing/events.rs
use crate::EventTrait;

#[derive(Debug, Clone)]
pub struct SubscriptionCreated {
    pub user_id: i64,
    pub subscription_id: i64,
    pub plan: String,
    pub provider: String,
}
impl EventTrait for SubscriptionCreated {
    fn event_name() -> &'static str { "SubscriptionCreated" }
}

#[derive(Debug, Clone)]
pub struct SubscriptionCancelled {
    pub user_id: i64,
    pub subscription_name: String,
    pub immediate: bool,
}
impl EventTrait for SubscriptionCancelled {
    fn event_name() -> &'static str { "SubscriptionCancelled" }
}

#[derive(Debug, Clone)]
pub struct InvoicePaid {
    pub user_id: i64,
    pub invoice_id: i64,
    pub amount_cents: i64,
    pub currency: String,
    pub provider: String,
}
impl EventTrait for InvoicePaid {
    fn event_name() -> &'static str { "InvoicePaid" }
}

#[derive(Debug, Clone)]
pub struct InvoicePaymentFailed {
    pub user_id: i64,
    pub invoice_id: i64,
    pub reason: String,
}
impl EventTrait for InvoicePaymentFailed {
    fn event_name() -> &'static str { "InvoicePaymentFailed" }
}

#[derive(Debug, Clone)]
pub struct TrialEnding {
    pub user_id: i64,
    pub subscription_id: i64,
    pub trial_ends_at: chrono::DateTime<chrono::Utc>,
}
impl EventTrait for TrialEnding {
    fn event_name() -> &'static str { "TrialEnding" }
}
```

- [ ] **Step 2: Webhook handler**

```rust
// framework/src/billing/webhook.rs
//! POST /webhooks/billing/{provider} — verify signature via PayRail,
//! normalize into `PaymentEvent`, react in our subscription state
//! machine, dispatch typed framework events.

use crate::billing::{events::*, payrail, subscription};
use crate::{json_response, Event, FrameworkError, Request, Response};
use payrail::{PaymentEvent, PaymentEventType, PaymentProvider, WebhookRequest};

pub fn handle(
    provider_name: &'static str,
) -> impl Fn(Request) -> futures::future::BoxFuture<'static, Response> + Clone {
    move |req: Request| {
        Box::pin(async move {
            // 1. Read headers + body
            let signature = req
                .header("stripe-signature")
                .or_else(|| req.header("paypal-transmission-sig"))
                .or_else(|| req.header("x-lipila-signature"))
                .unwrap_or_default();
            let raw_body = req.into_body_bytes().await?;

            // 2. Map provider name → PaymentProvider
            let provider = match provider_name {
                "stripe" => PaymentProvider::Stripe,
                "paypal" => PaymentProvider::PayPal,
                "lipila" => PaymentProvider::Lipila,
                other => PaymentProvider::other(other),
            };

            // 3. Verify + normalize via PayRail
            let client = payrail()?;
            let webhook_request = WebhookRequest::new(provider.clone(), &raw_body, &signature);
            let event: PaymentEvent = client
                .verify_and_normalize_webhook(webhook_request)
                .await
                .map_err(|e| FrameworkError::internal(format!("webhook verify: {}", e)))?;

            // 4. React in our state machine + dispatch typed events
            handle_normalized_event(event).await?;
            json_response!({ "received": true })
        })
    }
}

async fn handle_normalized_event(event: PaymentEvent) -> Result<(), FrameworkError> {
    let provider_str = format!("{:?}", event.provider());

    match event.event_type() {
        PaymentEventType::Succeeded => {
            // Look up the user + subscription via merchant_reference or
            // provider_reference. Implementer wires the lookup.
            let invoice_id = persist_invoice_paid(&event).await?;
            let _ = Event::dispatch(InvoicePaid {
                user_id: 0, // populated from lookup
                invoice_id,
                amount_cents: event.amount().map(|m| m.minor_amount().value() as i64).unwrap_or(0),
                currency: event.amount().map(|m| m.currency().to_string()).unwrap_or_default(),
                provider: provider_str,
            })
            .await;
        }
        PaymentEventType::Failed | PaymentEventType::Cancelled => {
            // Subscription transitions to past_due; dispatch InvoicePaymentFailed.
            let invoice_id = persist_invoice_failed(&event).await?;
            let _ = Event::dispatch(InvoicePaymentFailed {
                user_id: 0, // populated from lookup
                invoice_id,
                reason: event.message().unwrap_or("payment failed").to_string(),
            })
            .await;
            // Optional: subscription::mark_past_due(...).await?;
        }
        _ => {
            // Other event types — pending, action-required, etc. Log
            // for now; expand the state machine as use cases emerge.
            tracing::debug!(?event, "unhandled payment event");
        }
    }
    Ok(())
}

async fn persist_invoice_paid(_event: &PaymentEvent) -> Result<i64, FrameworkError> {
    unimplemented!("UPDATE invoices SET status='paid' WHERE provider_invoice_id = ?")
}
async fn persist_invoice_failed(_event: &PaymentEvent) -> Result<i64, FrameworkError> {
    unimplemented!("UPDATE invoices SET status='open' (and log failure) WHERE provider_invoice_id = ?")
}
```

> **PayRail webhook surface:** Verify the exact `WebhookRequest::new` + `verify_and_normalize_webhook` method on PayRailClient via `reference/payrail-0.1.5/crates/payrail/src/core/webhook.rs` + `client.rs` + the per-provider modules. If PayRail exposes a different facade (e.g. per-provider `client.stripe().handle_webhook(...)`), adapt the bridge.

- [ ] **Step 3: Test + commit**

```rust
// framework/tests/billing.rs — append
#[tokio::test]
async fn webhook_dispatches_invoice_paid_event() {
    // Construct a PaymentEvent in-process (bypassing signature verify
    // by calling handle_normalized_event directly), assert that
    // Event::fake() recorded an InvoicePaid dispatch.
    let _g = Event::fake();
    let event = make_succeeded_event();
    suprnova::billing::webhook::handle_normalized_event_for_test(event).await.unwrap();
    suprnova::assert_dispatched::<InvoicePaid>(|e| e.amount_cents > 0);
}
```

> **Test helper:** Add a `pub(crate) fn handle_normalized_event_for_test` re-export so tests can call the handler without going through the HTTP boundary.

```bash
git add framework/src/billing/webhook.rs framework/src/billing/events.rs framework/tests/billing.rs
git commit -m "feat(billing): webhook handler bridges PayRail PaymentEvent → typed framework events"
```

---

## Task 5: Period-end charging job

**Files:** `framework/src/billing/period_job.rs`

When `current_period_end` rolls past `NOW()` on an active subscription, the framework dispatches a `ChargeSubscriptionJob` to the Phase 5 queue. The job uses PayRail to charge the next period; on success it advances `current_period_start/end`; on failure it transitions to `past_due` and dispatches `InvoicePaymentFailed`.

- [ ] **Step 1: Implement**

```rust
// framework/src/billing/period_job.rs
//! Scheduled job that finds subscriptions whose period has rolled
//! over and charges the next period via PayRail.

use crate::billing::{events::*, payrail, subscription::Subscription};
use crate::{async_trait, Event, FrameworkError, Job};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ChargeSubscriptionJob {
    pub subscription_id: i64,
}

#[async_trait]
impl Job for ChargeSubscriptionJob {
    fn job_name() -> &'static str { "ChargeSubscription" }

    async fn handle(self) -> Result<(), FrameworkError> {
        let sub = load_subscription(self.subscription_id).await?;
        if !sub.is_active() || sub.is_on_trial() {
            return Ok(());
        }

        let _client = payrail()?;
        // Build a CreatePaymentRequest, send through PayRail.
        // Implementer: fill in via payrail::CreatePaymentRequest::builder()
        // pulling Money / Customer / idempotency from sub fields.
        match charge_next_period(&sub).await {
            Ok(invoice_id) => {
                advance_period(&sub).await?;
                let _ = Event::dispatch(InvoicePaid {
                    user_id: sub.user_id,
                    invoice_id,
                    amount_cents: 0, // populated from charge result
                    currency: "USD".into(),
                    provider: format!("{:?}", sub.provider),
                })
                .await;
            }
            Err(err) => {
                mark_past_due(&sub).await?;
                let _ = Event::dispatch(InvoicePaymentFailed {
                    user_id: sub.user_id,
                    invoice_id: 0,
                    reason: err.to_string(),
                })
                .await;
            }
        }
        Ok(())
    }
}

async fn load_subscription(_id: i64) -> Result<Subscription, FrameworkError> {
    unimplemented!()
}
async fn charge_next_period(_sub: &Subscription) -> Result<i64, FrameworkError> {
    unimplemented!("construct payrail::CreatePaymentRequest + client.create_payment(...)")
}
async fn advance_period(_sub: &Subscription) -> Result<(), FrameworkError> {
    unimplemented!()
}
async fn mark_past_due(_sub: &Subscription) -> Result<(), FrameworkError> {
    unimplemented!()
}
```

- [ ] **Step 2: Schedule it**

```rust
// In bootstrap.rs:
suprnova::Schedule::call(|| async {
    // Find subscriptions whose current_period_end < NOW() and status = 'active'.
    let due = suprnova::billing::period_job::find_due().await?;
    for sub_id in due {
        suprnova::Queue::push(suprnova::billing::period_job::ChargeSubscriptionJob {
            subscription_id: sub_id,
        })
        .await?;
    }
    Ok::<_, suprnova::FrameworkError>(())
})
.at("every hour");
```

- [ ] **Step 3: Commit**

```bash
git add framework/src/billing/period_job.rs
git commit -m "feat(billing): ChargeSubscriptionJob + hourly Schedule that picks up rolled-over periods"
```

---

## Task 6: Invoice PDF rendering

**Files:** `framework/src/billing/invoice.rs`

Same printpdf integration as the original plan; the `Invoice` type now references PayRail's `Money` for currency-correctness.

- [ ] **Step 1: Implement**

```rust
// framework/src/billing/invoice.rs
use crate::FrameworkError;
use chrono::NaiveDate;
use payrail::Money;
use printpdf::*;
use serde::{Deserialize, Serialize};
use std::io::BufWriter;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Invoice {
    pub id: i64,
    pub user_id: i64,
    pub number: String,
    pub date: NaiveDate,
    pub customer_name: String,
    pub customer_email: String,
    pub line_items: Vec<LineItem>,
    pub currency: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LineItem {
    pub description: String,
    pub quantity: u32,
    pub unit_price_cents: i64,
}

impl LineItem {
    pub fn total_cents(&self) -> i64 {
        self.quantity as i64 * self.unit_price_cents
    }
}

impl Invoice {
    pub fn total_cents(&self) -> i64 {
        self.line_items.iter().map(|i| i.total_cents()).sum()
    }

    pub fn total_money(&self) -> Result<Money, FrameworkError> {
        let currency = self
            .currency
            .parse::<payrail::CurrencyCode>()
            .map_err(|e| FrameworkError::internal(format!("currency: {}", e)))?;
        let minor = payrail::MinorAmount::new(self.total_cents());
        Ok(Money::new(minor, currency))
    }

    pub fn render_pdf(&self) -> Result<Vec<u8>, FrameworkError> {
        let (doc, page1, layer1) = PdfDocument::new(
            format!("Invoice {}", self.number),
            Mm(210.0),
            Mm(297.0),
            "Layer 1",
        );
        let font = doc
            .add_builtin_font(BuiltinFont::HelveticaBold)
            .map_err(|e| FrameworkError::internal(format!("pdf font: {}", e)))?;
        let body_font = doc
            .add_builtin_font(BuiltinFont::Helvetica)
            .map_err(|e| FrameworkError::internal(format!("pdf font: {}", e)))?;
        let layer = doc.get_page(page1).get_layer(layer1);

        layer.use_text(format!("INVOICE {}", self.number), 24.0, Mm(20.0), Mm(270.0), &font);
        layer.use_text(format!("Date: {}", self.date.format("%Y-%m-%d")), 12.0, Mm(20.0), Mm(255.0), &body_font);
        layer.use_text(
            format!("Bill to: {} <{}>", self.customer_name, self.customer_email),
            12.0, Mm(20.0), Mm(245.0), &body_font,
        );

        let mut y = Mm(220.0);
        for item in &self.line_items {
            let line = format!(
                "{} × {}    {:.2} {}    {:.2} {}",
                item.quantity, item.description,
                item.unit_price_cents as f64 / 100.0, self.currency,
                item.total_cents() as f64 / 100.0, self.currency,
            );
            layer.use_text(line, 11.0, Mm(20.0), y, &body_font);
            y = Mm(y.0 - 8.0);
        }

        let total = format!("TOTAL: {:.2} {}", self.total_cents() as f64 / 100.0, self.currency);
        layer.use_text(total, 14.0, Mm(20.0), Mm(50.0), &font);

        let mut buf = Vec::new();
        {
            let mut writer = BufWriter::new(&mut buf);
            doc.save(&mut writer)
                .map_err(|e| FrameworkError::internal(format!("pdf save: {}", e)))?;
        }
        Ok(buf)
    }
}

pub(crate) async fn create_one_off<B: crate::Billable + ?Sized>(
    _user: &B,
    _provider: payrail::PaymentProvider,
    _amount_cents: i64,
    _currency: &str,
    _description: &str,
) -> Result<Invoice, FrameworkError> {
    unimplemented!("payrail::CreatePaymentRequest + persist Invoice row")
}
```

- [ ] **Step 2: Test + commit**

```rust
// framework/tests/billing.rs — append
#[test]
fn invoice_pdf_produces_nonzero_bytes() {
    let inv = suprnova::billing::Invoice {
        id: 1,
        user_id: 1,
        number: "INV-001".into(),
        date: chrono::NaiveDate::from_ymd_opt(2026, 5, 14).unwrap(),
        customer_name: "Alice".into(),
        customer_email: "alice@example.com".into(),
        line_items: vec![suprnova::billing::LineItem {
            description: "Pro plan".into(),
            quantity: 1,
            unit_price_cents: 1900,
        }],
        currency: "USD".into(),
    };
    let pdf = inv.render_pdf().unwrap();
    assert!(pdf.len() > 1000);
    assert_eq!(&pdf[..4], b"%PDF");
}
```

```bash
cargo test -p suprnova --test billing invoice_pdf
git add framework/src/billing/invoice.rs framework/tests/billing.rs
git commit -m "feat(billing): Invoice + LineItem + render_pdf via printpdf (uses payrail Money)"
```

---

## Task 7: Customer-portal URL builder

**Files:** `framework/src/billing/portal.rs`

PayRail doesn't ship a unified billing-portal API; each provider implements its own. We wrap them per provider.

- [ ] **Step 1: Implement**

```rust
// framework/src/billing/portal.rs
//! Customer-portal URL builders, dispatched per provider.

use crate::{Billable, FrameworkError};
use payrail::PaymentProvider;

pub(crate) async fn url_for<B: Billable + ?Sized>(
    user: &B,
    provider: PaymentProvider,
    return_url: &str,
) -> Result<String, FrameworkError> {
    let customer_id = user
        .provider_customer_id(&provider)
        .await
        .ok_or_else(|| FrameworkError::internal("no provider customer id on user"))?;
    match provider {
        PaymentProvider::Stripe => stripe_portal(&customer_id, return_url).await,
        PaymentProvider::PayPal => paypal_portal(&customer_id, return_url).await,
        _ => Err(FrameworkError::internal(format!(
            "customer portal not supported for {:?}",
            provider
        ))),
    }
}

async fn stripe_portal(customer_id: &str, return_url: &str) -> Result<String, FrameworkError> {
    // Stripe has a billing_portal/sessions endpoint. Call directly via
    // Http:: (Phase 2) — PayRail doesn't expose a portal API today.
    let secret = std::env::var("STRIPE_SECRET_KEY")
        .map_err(|_| FrameworkError::internal("STRIPE_SECRET_KEY not set"))?;
    let resp = crate::Http::post("https://api.stripe.com/v1/billing_portal/sessions")
        .with_token(&secret)
        .form(&[("customer", customer_id), ("return_url", return_url)])
        .send()
        .await?;
    let body: serde_json::Value = resp.json().await?;
    body["url"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| FrameworkError::internal(format!("stripe portal response: {}", body)))
}

async fn paypal_portal(_customer_id: &str, _return_url: &str) -> Result<String, FrameworkError> {
    // PayPal redirects to https://www.paypal.com/myaccount/autopay/
    // — there's no per-customer portal-session API. Return a static
    // URL, optionally with the return_url as a query param the user's
    // app handles after they navigate back.
    Ok("https://www.paypal.com/myaccount/autopay/".to_string())
}
```

- [ ] **Step 2: Commit**

```bash
git add framework/src/billing/portal.rs
git commit -m "feat(billing): customer-portal URL builders per provider (Stripe via Http:: facade)"
```

---

## Task 8: App dogfood

**Files:** `app/src/listeners/billing_listener.rs`, route wiring

- [ ] **Step 1: BillingListener**

```rust
// app/src/listeners/billing_listener.rs
use suprnova::billing::events::{InvoicePaid, SubscriptionCancelled, TrialEnding};
use suprnova::{async_trait, events::Listener, FrameworkError};
use tracing::info;

pub struct BillingListener;

#[async_trait]
impl Listener<InvoicePaid> for BillingListener {
    async fn handle(&self, event: &InvoicePaid) -> Result<(), FrameworkError> {
        info!(user_id = event.user_id, amount = event.amount_cents, "invoice paid");
        // Queue a receipt email (uses Phase 5 Mail + Queue)
        suprnova::Queue::push(crate::jobs::SendReceiptEmailJob {
            user_id: event.user_id,
            invoice_id: event.invoice_id,
            amount_cents: event.amount_cents,
            currency: event.currency.clone(),
        })
        .await?;
        Ok(())
    }
}

#[async_trait]
impl Listener<SubscriptionCancelled> for BillingListener {
    async fn handle(&self, event: &SubscriptionCancelled) -> Result<(), FrameworkError> {
        info!(user_id = event.user_id, immediate = event.immediate, "subscription cancelled");
        Ok(())
    }
}

#[async_trait]
impl Listener<TrialEnding> for BillingListener {
    async fn handle(&self, event: &TrialEnding) -> Result<(), FrameworkError> {
        info!(user_id = event.user_id, ?event.trial_ends_at, "trial ending soon");
        // Queue a "your trial is ending" reminder
        Ok(())
    }
}
```

- [ ] **Step 2: Register listener + webhook routes + portal route**

```rust
// app/src/bootstrap.rs
use std::sync::Arc;
suprnova::Event::listen::<suprnova::billing::events::InvoicePaid>(Arc::new(crate::listeners::BillingListener)).await;
suprnova::Event::listen::<suprnova::billing::events::SubscriptionCancelled>(Arc::new(crate::listeners::BillingListener)).await;
suprnova::Event::listen::<suprnova::billing::events::TrialEnding>(Arc::new(crate::listeners::BillingListener)).await;

// Routes:
//   post!("/webhooks/billing/stripe", suprnova::billing::webhook::handle("stripe"))
//   post!("/webhooks/billing/paypal", suprnova::billing::webhook::handle("paypal"))
//   post!("/webhooks/billing/lipila", suprnova::billing::webhook::handle("lipila"))
//   get!("/billing/portal", controllers::billing::portal)
```

- [ ] **Step 3: Smoke test**

```bash
cargo run -p app -- serve &
sleep 2
# Send a test Stripe webhook (use stripe-cli locally or a fixture)
stripe trigger checkout.session.completed
kill %1
```

- [ ] **Step 4: Commit**

```bash
git add app/src
git commit -m "feat(app): BillingListener + webhook routes + portal endpoint"
```

---

## Task 9: Workspace lint + roadmap update

```bash
cargo clippy --workspace --features billing-all -- -D warnings
cargo test --workspace --features billing-all
```

ROADMAP "Where we are" — move to Production-ready:
- Billing (subscriptions + invoices + customer portal) via PayRail
- 13 payment providers via PayRail (Stripe, PayPal, Lipila, MTN MoMo, M-Pesa, Airtel, Orange Money, Flutterwave, Paystack, Circle, Coinbase, Bridge, Binance)

Commit + push.

---

## Out of v1 scope

- **Paddle.** Not in PayRail's `BuiltinProvider` enum and PayRail doesn't expose a provider extension trait. Path forward if a consumer needs Paddle: (a) fork PayRail into the workspace as `suprnova-payrail` and add a `Paddle` variant + provider module, or (b) contribute upstream. Documented; not implemented.
- **Tax computation.** Stripe Tax / Paddle's MoR model handles this — consumers configure on the provider side. We don't compute tax ourselves.
- **Dunning automation beyond status-only.** We mark `past_due` and dispatch `InvoicePaymentFailed`; the app's listener decides what to do (email retry, account suspension). Sophisticated dunning (multi-step retry schedules, grace periods, payment-method-update reminders) is consumer-app concern.

---

## Self-Review

| Spec item | Covered by | Source |
|---|---|---|
| PayRail bootstrap + provider configuration | Task 3 | PayRail |
| Billable user trait | Task 2 | Ours |
| Subscription model + state machine | Task 2 | Ours |
| Provider customer creation | Task 2 | PayRail |
| Webhook signature verification + normalization | Task 4 | PayRail |
| Typed subscription events | Task 4 | Ours (Phase 1 EventDispatcher) |
| Period-end charging job | Task 5 | Ours + Phase 5 Queue + Schedule |
| Invoice PDF | Task 6 | Ours (printpdf) |
| Customer portal URL builders | Task 7 | Ours (per-provider HTTP via Phase 2) |
| App dogfood (listeners + webhook routes) | Task 8 | — |

**Architectural correctness:** PayRail owns the payment-rails layer (charges, refunds, captures, webhook normalization, multi-provider routing). We own the subscription business logic (lifecycle, invoices, period-end charging, portal redirects). The bridge between them is `webhook.rs` (normalize → react → dispatch typed events) and `period_job.rs` (state machine → PayRail charge).

**Placeholder scan:** Concrete `> verification:` notes flag specific files to read in PayRail before implementation. SeaORM persistence stubs (`persist_new`, `update`, `find_by_user_name`) are explicitly marked `unimplemented!()` so implementers wire them as proper ActiveModel calls; they're not silent gaps.

---

## Execution Handoff

**Subagent-Driven recommended per task. Tasks 2 (Subscription model) and Task 4 (Webhook handler) are the largest pieces — give each its own agent with full PayRail context.**
