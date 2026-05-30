# Payments — Frontend Integration

The server returns a `SessionPayload` as part of your Inertia page props. The payload carries a `flow` field that tells the frontend which widget to mount; your frontend dispatches on `flow` and never names a specific provider. This chapter covers the Svelte 5, React 19, and Vue 3.5 dispatch loops, including the Stripe Elements confirm-card-payment cycle and the off-session 3DS step-up handler.

The five possible `flow` values and their associated fields:

| `flow` | Fields | Widget |
|---|---|---|
| `stripe_elements` | `client_secret`, `publishable_key`, `provider_session_id` | Stripe Elements (embedded card form) |
| `stripe_checkout_redirect` | `url`, `provider_session_id` | Redirect to Stripe-hosted checkout |
| `paddle_inline` | `transaction_id`, `client_token`, `customer_token?` | Paddle.js inline overlay |
| `mobile_money_prompt` | `provider_transaction_id`, `message`, `operator` | USSD / operator-app prompt + polling |
| `redirect` | `url`, `provider_session_id` | Generic redirect (Mollie, mock, etc.) |

The backend controller calls `Checkout::start_session` and returns the result as Inertia props — from the frontend's perspective the API is the same regardless of which adapter is running.

## Dispatch on `flow`, not on provider

Your checkout page reads the `flow` field once and mounts the matching widget. It never names "Stripe" or "Paddle"; only the bootstrap that chose the adapter knows. This is the contract the rest of the chapter builds on.

### Why Suprnova diverges

Laravel Cashier ships a Blade view for Stripe Checkout, a partials path for SCA, and a separate SDK convention for Paddle. The Stripe and Paddle paths don't share a frontend contract — each provider's widget is wired to a different controller action and a different template tree.

Suprnova flips that: the backend always returns the same `SessionPayload` enum and the frontend always switches on `flow`. Adding a new provider means adding one variant on the server side and one `case` on the client side; the rest of your checkout page does not move. The Mobile Money variant is the proof — it produces no widget at all (the customer confirms on their phone), and the dispatcher absorbs it without any special-casing in the calling component.

## Svelte 5

```svelte
<!-- src/pages/Billing/Checkout.svelte -->
<script lang="ts">
  import { page } from "@inertiajs/svelte";

  // SessionPayload arrives in Inertia page props
  let session = $derived($page.props.session as SessionPayload);

  type MobileMoneyOperator =
    | { kind: "mtn_momo" }
    | { kind: "mpesa" }
    | { kind: "airtel_money" }
    | { kind: "orange_money" }
    | { kind: "lipila" }
    | { kind: "custom"; identifier: string };

  type SessionPayload =
    | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
    | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
    | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
    | { flow: "mobile_money_prompt"; provider_transaction_id: string; message: string; operator: MobileMoneyOperator }
    | { flow: "redirect"; url: string; provider_session_id: string };

  let mobileMessage = $state("");

  $effect(() => {
    if (!session) return;
    switch (session.flow) {
      case "stripe_elements":
        mountStripeElements(session);
        break;
      case "stripe_checkout_redirect":
        window.location.href = session.url;
        break;
      case "paddle_inline":
        mountPaddleInline(session);
        break;
      case "mobile_money_prompt":
        mobileMessage = session.message;
        pollMobileMoney(session.provider_transaction_id);
        break;
      case "redirect":
        window.location.href = session.url;
        break;
    }
  });

  async function mountStripeElements(s: Extract<SessionPayload, { flow: "stripe_elements" }>) {
    // Stripe.js must be loaded — add to index.html:
    // <script src="https://js.stripe.com/v3/"></script>
    const stripe = (window as any).Stripe(s.publishable_key);
    const elements = stripe.elements({ clientSecret: s.client_secret });

    const card = elements.create("card");
    card.mount("#card-element");

    // Wire up form submission:
    const form = document.getElementById("payment-form") as HTMLFormElement;
    form?.addEventListener("submit", async (e) => {
      e.preventDefault();
      const { error, paymentIntent } = await stripe.confirmCardPayment(s.client_secret, {
        payment_method: { card },
      });
      if (error) {
        // Show error to user
        console.error(error.message);
      } else if (paymentIntent?.status === "succeeded") {
        // Payment complete — navigate or show confirmation
        window.location.href = "/billing/success";
      }
    });
  }

  function mountPaddleInline(s: Extract<SessionPayload, { flow: "paddle_inline" }>) {
    // Paddle.js must be loaded — add to index.html:
    // <script src="https://cdn.paddle.com/paddle/v2/paddle.js"></script>
    const Paddle = (window as any).Paddle;
    Paddle.Initialize({ token: s.client_token });
    Paddle.Checkout.open({
      transactionId: s.transaction_id,
      customerToken: s.customer_token,
    });
  }

  async function pollMobileMoney(txId: string) {
    // Poll your own backend, which reads the mirror transactions table.
    // The webhook handler updates the row when the provider notifies us.
    const deadline = Date.now() + 5 * 60_000;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 3000));
      const res = await fetch(`/billing/status?transaction_id=${encodeURIComponent(txId)}`);
      const { status } = await res.json();
      if (status === "succeeded") {
        window.location.href = "/billing/success";
        return;
      }
      if (status === "failed" || status === "canceled" || status === "expired") {
        window.location.href = "/billing/failed";
        return;
      }
    }
  }
</script>

<div id="payment-form">
  <div id="card-element"></div>
  <!-- Only rendered for stripe_elements; hidden otherwise -->
  {#if session?.flow === "stripe_elements"}
    <button type="submit">Pay now</button>
  {/if}
  {#if session?.flow === "mobile_money_prompt"}
    <p>{mobileMessage}</p>
    <p>Waiting for confirmation…</p>
  {/if}
</div>
```

## React 19

```tsx
// src/pages/Billing/Checkout.tsx
import { useEffect, useRef, useState } from "react";
import { usePage } from "@inertiajs/react";

type MobileMoneyOperator =
  | { kind: "mtn_momo" }
  | { kind: "mpesa" }
  | { kind: "airtel_money" }
  | { kind: "orange_money" }
  | { kind: "lipila" }
  | { kind: "custom"; identifier: string };

type SessionPayload =
  | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
  | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
  | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
  | { flow: "mobile_money_prompt"; provider_transaction_id: string; message: string; operator: MobileMoneyOperator }
  | { flow: "redirect"; url: string; provider_session_id: string };

export default function Checkout() {
  const { session } = usePage<{ session: SessionPayload }>().props;
  const mountedRef = useRef(false);
  const [mobileMessage, setMobileMessage] = useState("");

  useEffect(() => {
    if (!session || mountedRef.current) return;
    mountedRef.current = true;

    switch (session.flow) {
      case "stripe_elements":
        mountStripeElements(session);
        break;
      case "stripe_checkout_redirect":
        window.location.href = session.url;
        break;
      case "paddle_inline":
        mountPaddleInline(session);
        break;
      case "mobile_money_prompt":
        setMobileMessage(session.message);
        pollMobileMoney(session.provider_transaction_id);
        break;
      case "redirect":
        window.location.href = session.url;
        break;
    }
  }, [session]);

  async function mountStripeElements(
    s: Extract<SessionPayload, { flow: "stripe_elements" }>
  ) {
    const stripe = (window as any).Stripe(s.publishable_key);
    const elements = stripe.elements({ clientSecret: s.client_secret });
    const card = elements.create("card");
    card.mount("#card-element");

    const form = document.getElementById("payment-form") as HTMLFormElement;
    form?.addEventListener("submit", async (e) => {
      e.preventDefault();
      const { error, paymentIntent } = await stripe.confirmCardPayment(s.client_secret, {
        payment_method: { card },
      });
      if (error) {
        console.error(error.message);
      } else if (paymentIntent?.status === "succeeded") {
        window.location.href = "/billing/success";
      }
    });
  }

  function mountPaddleInline(
    s: Extract<SessionPayload, { flow: "paddle_inline" }>
  ) {
    const Paddle = (window as any).Paddle;
    Paddle.Initialize({ token: s.client_token });
    Paddle.Checkout.open({
      transactionId: s.transaction_id,
      customerToken: s.customer_token,
    });
  }

  async function pollMobileMoney(txId: string) {
    const deadline = Date.now() + 5 * 60_000;
    while (Date.now() < deadline) {
      await new Promise((r) => setTimeout(r, 3000));
      const res = await fetch(`/billing/status?transaction_id=${encodeURIComponent(txId)}`);
      const { status } = await res.json();
      if (status === "succeeded") {
        window.location.href = "/billing/success";
        return;
      }
      if (status === "failed" || status === "canceled" || status === "expired") {
        window.location.href = "/billing/failed";
        return;
      }
    }
  }

  return (
    <form id="payment-form">
      <div id="card-element" />
      {session?.flow === "stripe_elements" && (
        <button type="submit">Pay now</button>
      )}
      {session?.flow === "mobile_money_prompt" && (
        <div>
          <p>{mobileMessage}</p>
          <p>Waiting for confirmation…</p>
        </div>
      )}
    </form>
  );
}
```

The `mountedRef` guard prevents double-mounting under React 19's StrictMode development double-render.

## Vue 3.5

```vue
<!-- src/pages/Billing/Checkout.vue -->
<script setup lang="ts">
import { onMounted, ref } from "vue";
import { usePage } from "@inertiajs/vue3";

type MobileMoneyOperator =
  | { kind: "mtn_momo" }
  | { kind: "mpesa" }
  | { kind: "airtel_money" }
  | { kind: "orange_money" }
  | { kind: "lipila" }
  | { kind: "custom"; identifier: string };

type SessionPayload =
  | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
  | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
  | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
  | { flow: "mobile_money_prompt"; provider_transaction_id: string; message: string; operator: MobileMoneyOperator }
  | { flow: "redirect"; url: string; provider_session_id: string };

const page = usePage<{ session: SessionPayload }>();
const session = page.props.session;
const isStripeElements = ref(session?.flow === "stripe_elements");
const isMobileMoney = ref(session?.flow === "mobile_money_prompt");
const mobileMessage = ref(
  session?.flow === "mobile_money_prompt" ? session.message : ""
);

onMounted(() => {
  if (!session) return;
  switch (session.flow) {
    case "stripe_elements":
      mountStripeElements(session);
      break;
    case "stripe_checkout_redirect":
      window.location.href = session.url;
      break;
    case "paddle_inline":
      mountPaddleInline(session);
      break;
    case "mobile_money_prompt":
      pollMobileMoney(session.provider_transaction_id);
      break;
    case "redirect":
      window.location.href = session.url;
      break;
  }
});

async function mountStripeElements(
  s: Extract<SessionPayload, { flow: "stripe_elements" }>
) {
  const stripe = (window as any).Stripe(s.publishable_key);
  const elements = stripe.elements({ clientSecret: s.client_secret });
  const card = elements.create("card");
  card.mount("#card-element");

  const form = document.getElementById("payment-form") as HTMLFormElement;
  form?.addEventListener("submit", async (e) => {
    e.preventDefault();
    const { error, paymentIntent } = await stripe.confirmCardPayment(s.client_secret, {
      payment_method: { card },
    });
    if (error) {
      console.error(error.message);
    } else if (paymentIntent?.status === "succeeded") {
      window.location.href = "/billing/success";
    }
  });
}

function mountPaddleInline(
  s: Extract<SessionPayload, { flow: "paddle_inline" }>
) {
  const Paddle = (window as any).Paddle;
  Paddle.Initialize({ token: s.client_token });
  Paddle.Checkout.open({
    transactionId: s.transaction_id,
    customerToken: s.customer_token,
  });
}

async function pollMobileMoney(txId: string) {
  const deadline = Date.now() + 5 * 60_000;
  while (Date.now() < deadline) {
    await new Promise((r) => setTimeout(r, 3000));
    const res = await fetch(`/billing/status?transaction_id=${encodeURIComponent(txId)}`);
    const { status } = await res.json();
    if (status === "succeeded") {
      window.location.href = "/billing/success";
      return;
    }
    if (status === "failed" || status === "canceled" || status === "expired") {
      window.location.href = "/billing/failed";
      return;
    }
  }
}
</script>

<template>
  <form id="payment-form">
    <div id="card-element" />
    <button v-if="isStripeElements" type="submit">Pay now</button>
    <div v-if="isMobileMoney">
      <p>{{ mobileMessage }}</p>
      <p>Waiting for confirmation…</p>
    </div>
  </form>
</template>
```

## Loading the Payment SDKs

Add the relevant scripts to your `index.html` (or equivalent entry point). Only include the ones your provider selection requires:

```html
<!-- Stripe (add if using stripe_elements or stripe_checkout_redirect) -->
<script src="https://js.stripe.com/v3/" crossorigin="anonymous"></script>

<!-- Paddle (add if using paddle_inline) -->
<script src="https://cdn.paddle.com/paddle/v2/paddle.js" crossorigin="anonymous"></script>
```

Both scripts are loaded asynchronously by the browser. If you're using Vite with code-splitting, load these via dynamic `import()` or include them as externals in your `vite.config.ts` to avoid bundling the provider SDKs yourself.

Stripe and Paddle both require you to load the SDK from their own CDN — Stripe makes this a PCI compliance condition, and Paddle relies on it for live URL rewriting. Subresource Integrity (`integrity="sha384-..."`) is not usable on either script because both vendors ship continuously and do not publish stable hashes; the trust boundary is the HTTPS connection plus the vendor's CDN. If your threat model requires SRI for everything you embed, that's a signal to keep all payment UI on a vendor-hosted checkout (`stripe_checkout_redirect`, or Paddle's hosted overlay invoked from a server-issued redirect) rather than in your own page.

## TypeScript Types

The `SessionPayload` type shown in each example above is a discriminated union matching the Rust enum's serialized form. You can generate it automatically with `suprnova generate-types` if your `SessionPayload` is exposed via a `#[derive(InertiaProps)]` wrapper, or define it manually as shown.

## Mobile Money Polling

`mobile_money_prompt` is the only flow where the customer never touches your page after the prompt arrives. They confirm on their phone (USSD menu or operator-app push), the provider notifies your webhook handler, and your frontend has to discover that the transaction settled.

Wire a small status endpoint that reads the mirror `payments_transactions` table by `provider_transaction_id`. The webhook handler installed by `webhook_routes(db)` keeps the row's status column current; your endpoint just reflects it back:

```rust,ignore
use suprnova::{Json, Query, json_response};
use suprnova::payments::entities::transaction;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

#[derive(serde::Deserialize)]
pub struct StatusQuery {
    pub transaction_id: String,
}

pub async fn status(Query(q): Query<StatusQuery>) -> Json<serde_json::Value> {
    let db = suprnova::db().await;
    let row = transaction::Entity::find()
        .filter(transaction::Column::ProviderTransactionId.eq(q.transaction_id))
        .one(&db)
        .await
        .unwrap();
    let status = row.map(|r| r.status).unwrap_or_else(|| "pending".into());
    Json(serde_json::json!({ "status": status }))
}
```

The frontend `pollMobileMoney` helper shown in each example above hits that endpoint every three seconds with a five-minute ceiling. Status strings come from the `PaymentStatus` enum and serialize as snake_case: `created`, `requires_action`, `pending`, `processing`, `authorized`, `expired`, `succeeded`, `failed`, `canceled`, `refunded`, `partially_refunded`, `disputed`.

## Error Handling — `RequiresClientAction`

When `Payment::charge` (server-side capture) returns `ChargeResult::RequiresClientAction`, the backend serializes the result to JSON and returns it to the frontend. This happens for off-session 3DS step-up flows where the card issuer demands additional authentication.

The JSON looks like:

```json
{
  "kind": "requires_client_action",
  "provider_transaction_id": "pi_...",
  "action_kind": "stripe_3ds",
  "client_secret": "pi_..._secret_...",
  "publishable_key": "pk_live_..."
}
```

`client_secret` and `publishable_key` are `Option<String>` on the Rust side and will be absent from the JSON when an action does not need them. Always null-check both before passing them to a provider SDK, and let `action_kind` drive the dispatch — that field is always present.

Your backend controller should detect this and return it as a distinct Inertia prop or as an HTTP response that the frontend reads. Example controller pattern:

```rust,ignore
use suprnova::payments::ChargeResult;

let result = payment.charge(req).await?;
match result {
    ChargeResult::Completed { .. } => {
        // Redirect to success page
    }
    ChargeResult::RequiresClientAction { action_kind, client_secret, publishable_key, .. } => {
        return inertia.render("Billing/ThreeDSChallenge", json!({
            "action_kind": action_kind,
            "client_secret": client_secret,
            "publishable_key": publishable_key,
        }));
    }
    ChargeResult::RedirectRequired { url, .. } => {
        // Redirect the browser
    }
}
```

On the frontend, dispatch on `action_kind`:

**Svelte 5:**

```svelte
<script lang="ts">
  import { page } from "@inertiajs/svelte";

  let props = $derived($page.props as {
    action_kind: string;
    client_secret?: string;
    publishable_key?: string;
  });

  $effect(() => {
    if (!props.action_kind) return;
    switch (props.action_kind) {
      case "stripe_3ds":
        handleStripe3DS(props.client_secret!, props.publishable_key!);
        break;
      default:
        console.warn("Unknown action_kind:", props.action_kind);
    }
  });

  async function handleStripe3DS(clientSecret: string, publishableKey: string) {
    const stripe = (window as any).Stripe(publishableKey);
    const { error, paymentIntent } = await stripe.handleNextAction({ clientSecret });
    if (error) {
      // Show 3DS failure message
    } else if (paymentIntent?.status === "succeeded") {
      window.location.href = "/billing/success";
    }
  }
</script>
```

**React 19:**

```tsx
import { usePage } from "@inertiajs/react";
import { useEffect } from "react";

export default function ThreeDSChallenge() {
  const { action_kind, client_secret, publishable_key } = usePage<{
    action_kind: string;
    client_secret?: string;
    publishable_key?: string;
  }>().props;

  useEffect(() => {
    if (!action_kind) return;
    if (action_kind === "stripe_3ds" && client_secret && publishable_key) {
      const stripe = (window as any).Stripe(publishable_key);
      stripe.handleNextAction({ clientSecret: client_secret }).then(
        ({ error, paymentIntent }: any) => {
          if (!error && paymentIntent?.status === "succeeded") {
            window.location.href = "/billing/success";
          }
        }
      );
    }
  }, [action_kind]);

  return <div>Completing payment authentication...</div>;
}
```

**Vue 3.5:**

```vue
<script setup lang="ts">
import { onMounted } from "vue";
import { usePage } from "@inertiajs/vue3";

const { action_kind, client_secret, publishable_key } = usePage<{
  action_kind: string;
  client_secret?: string;
  publishable_key?: string;
}>().props;

onMounted(async () => {
  if (action_kind === "stripe_3ds" && client_secret && publishable_key) {
    const stripe = (window as any).Stripe(publishable_key);
    const { error, paymentIntent } = await stripe.handleNextAction({
      clientSecret: client_secret,
    });
    if (!error && paymentIntent?.status === "succeeded") {
      window.location.href = "/billing/success";
    }
  }
});
</script>

<template>
  <p>Completing payment authentication...</p>
</template>
```

The `action_kind` field is a provider-specific string. Currently `"stripe_3ds"` is the only value produced by the shipped Stripe adapter. When additional adapters require client actions, they will add their own `action_kind` values following the same pattern — write a default branch (`console.warn("Unknown action_kind:", k)`) so an unrecognised value fails loud rather than silently dropping the payment.

## Next

- [Payments](payments.md) — the five-trait surface, the registry, and the bootstrap pattern that produces the `SessionPayload`.
- [Payments — Stripe](payments-stripe.md) — server-side configuration for the `stripe_elements`, `stripe_checkout_redirect`, and `stripe_3ds` flows.
- [Payments — Paddle](payments-paddle.md) — server-side configuration for the `paddle_inline` flow and the Merchant-of-Record responsibility split.
- [Payments — Provider Guide](payments-provider-guide.md) — add a new `SessionPayload` variant when you write an adapter for a gateway Suprnova doesn't ship.
- [Frontend](frontend.md) — Inertia page setup, prop typing, and how `usePage` plugs into your Svelte / React / Vue starter.
