# suprnova-payments-stripe

Stripe reference adapter for [Suprnova](https://github.com/entrepeneur4lyf/suprnova)'s
generic Payments surface.

This crate implements the framework's provider-agnostic payment traits
(`Checkout`, `Payment`, `Subscription`, `CustomerStore`, `WebhookHandler`)
against the Stripe API via [`async-stripe`](https://crates.io/crates/async-stripe)
1.0.0-rc.5. Stripe is a payment gateway — it exposes server-side charges and
refunds, so the full `Payment` trait is implemented (unlike Paddle — see
`suprnova-payments-paddle`).

## What you get

- `StripeProvider` — implements `PaymentProvider`, the umbrella trait
  that aggregates all five capability traits
- `Checkout` — Checkout Sessions API, returns hosted-page URLs for
  one-shot payments and subscription enrollments
- `Payment` — server-side captures, refunds, and intent retrieval via
  PaymentIntents
- `Subscription` — create/update/cancel/resume against the Subscriptions API
- `CustomerStore` — `Customer` create + retrieve with mirror to the
  framework's `customers` table
- `WebhookHandler` — signature verification + idempotent event ingest via
  the framework's `payment_webhook_events` UNIQUE index
- `stripe_event_to_neutral` — mapping from Stripe event strings to the
  framework's neutral `PaymentEvent` enum

## Install

```toml
[dependencies]
suprnova-payments-stripe = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

## Usage

```rust,no_run
use suprnova_payments_stripe::StripeProvider;

let provider = StripeProvider::new(
    "sk_test_...",       // secret key
    "pk_test_...",       // publishable key (used by the client)
    "whsec_...",         // webhook signing secret
);

// Register with the framework's payment registry (in bootstrap)
suprnova::payments::register_provider("stripe", Box::new(provider));
```

See `manual/payments-stripe.md` in the Suprnova repo for the full guide,
including DB migrations, webhook endpoint setup, and the Inertia
billing-page wiring.

## License

MIT
