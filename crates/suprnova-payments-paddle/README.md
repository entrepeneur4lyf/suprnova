# suprnova-payments-paddle

Paddle reference adapter for [Suprnova](https://github.com/entrepeneur4lyf/suprnova)'s
generic Payments surface.

Paddle is a **Merchant of Record** — it owns subscription lifecycle, tax,
dunning, and chargebacks. Consequently, Paddle does NOT expose server-side
capture. This adapter implements the framework's `Checkout`,
`Subscription`, `CustomerStore`, and `WebhookHandler` traits, but
**intentionally does NOT implement `Payment`** —
`PaymentProvider::as_payment()` returns `None`, and an integration test
enforces this invariant.

Subscriptions are created **indirectly via checkout completion**: your
domain code calls `Checkout::start_session`, and the resulting
`subscription_id` arrives via the `SubscriptionCreated` webhook.
`Subscription::subscribe` and `Subscription::update` return
`PaymentError::NotSupported` with a clear migration message pointing at
the checkout flow.

For server-side capture / refunds, use `suprnova-payments-stripe` instead.

## What you get

- `PaddleProvider` — implements `PaymentProvider`; `as_payment()` returns `None`
- `Checkout` — Paddle's hosted Checkout, returns transaction URLs
- `Subscription` — cancel/resume/pause; create + plan-switch are
  webhook-driven (see above)
- `CustomerStore` — customer create + retrieve with mirror to the
  framework's `customers` table
- `WebhookHandler` — signature verification + idempotent event ingest via
  the framework's `payment_webhook_events` UNIQUE index
- `paddle_event_to_neutral` — mapping from Paddle event strings to the
  framework's neutral `PaymentEvent` enum

## Install

```toml
[dependencies]
suprnova-payments-paddle = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

## Usage

```rust,no_run
use suprnova_payments_paddle::PaddleProvider;

let provider = PaddleProvider::new(
    "pdl_live_apikey_...",   // API key
    "pdl_ntfset_...",        // notification (webhook) signing secret
    suprnova_payments_paddle::Environment::Production,
);

suprnova::payments::register_provider("paddle", Box::new(provider));
```

See `manual/payments-paddle.md` in the Suprnova repo for the full guide,
including MoR-specific UX patterns (tax disclosure, MoR statement,
chargeback handling).

## License

MIT
