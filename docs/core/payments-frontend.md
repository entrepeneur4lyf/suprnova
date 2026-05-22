# Payments — Frontend Integration

The server returns a `SessionPayload` as part of your Inertia page props. The payload carries a `flow` field that tells the frontend which widget to mount. Your frontend dispatches on `flow` and never needs to know which payment provider the backend selected.

The four possible `flow` values and their associated fields:

| `flow` | Fields | Widget |
|---|---|---|
| `stripe_elements` | `client_secret`, `publishable_key`, `provider_session_id` | Stripe Elements (embedded card form) |
| `stripe_checkout_redirect` | `url`, `provider_session_id` | Redirect to Stripe-hosted checkout |
| `paddle_inline` | `transaction_id`, `client_token`, `customer_token?` | Paddle.js inline overlay |
| `redirect` | `url`, `provider_session_id` | Generic redirect (Mollie, mock, etc.) |

The backend controller calls `Checkout::start_session` and returns the result as Inertia props — from the frontend's perspective the API is the same regardless of which adapter is running.

## Svelte 5

```svelte
<!-- src/pages/Billing/Checkout.svelte -->
<script lang="ts">
  import { page } from "@inertiajs/svelte";

  // SessionPayload arrives in Inertia page props
  let session = $derived($page.props.session as SessionPayload);

  type SessionPayload =
    | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
    | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
    | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
    | { flow: "redirect"; url: string; provider_session_id: string };

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
</script>

<div id="payment-form">
  <div id="card-element"></div>
  <!-- Only rendered for stripe_elements; hidden otherwise -->
  {#if session?.flow === "stripe_elements"}
    <button type="submit">Pay now</button>
  {/if}
</div>
```

## React 19

```tsx
// src/pages/Billing/Checkout.tsx
import { useEffect, useRef } from "react";
import { usePage } from "@inertiajs/react";

type SessionPayload =
  | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
  | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
  | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
  | { flow: "redirect"; url: string; provider_session_id: string };

export default function Checkout() {
  const { session } = usePage<{ session: SessionPayload }>().props;
  const mountedRef = useRef(false);

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

  return (
    <form id="payment-form">
      <div id="card-element" />
      {session?.flow === "stripe_elements" && (
        <button type="submit">Pay now</button>
      )}
    </form>
  );
}
```

The `mountedRef` guard prevents double-mounting in React 19's strict mode development double-render.

## Vue 3.5

```vue
<!-- src/pages/Billing/Checkout.vue -->
<script setup lang="ts">
import { onMounted, ref } from "vue";
import { usePage } from "@inertiajs/vue3";

type SessionPayload =
  | { flow: "stripe_elements"; client_secret: string; publishable_key: string; provider_session_id: string }
  | { flow: "stripe_checkout_redirect"; url: string; provider_session_id: string }
  | { flow: "paddle_inline"; transaction_id: string; client_token: string; customer_token?: string }
  | { flow: "redirect"; url: string; provider_session_id: string };

const page = usePage<{ session: SessionPayload }>();
const session = page.props.session;
const isStripeElements = ref(session?.flow === "stripe_elements");

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
</script>

<template>
  <form id="payment-form">
    <div id="card-element" />
    <button v-if="isStripeElements" type="submit">Pay now</button>
  </form>
</template>
```

## Loading the Payment SDKs

Add the relevant scripts to your `index.html` (or equivalent entry point). Only include the ones your provider selection requires:

```html
<!-- Stripe (add if using stripe_elements or stripe_checkout_redirect) -->
<script src="https://js.stripe.com/v3/"></script>

<!-- Paddle (add if using paddle_inline) -->
<script src="https://cdn.paddle.com/paddle/v2/paddle.js"></script>
```

Both scripts are loaded asynchronously by the browser. If you're using Vite with code-splitting, load these via dynamic `import()` or include them as externals in your `vite.config.ts` to avoid bundling the provider SDKs yourself.

## TypeScript Types

The `SessionPayload` type shown in each example above is a discriminated union matching the Rust enum's serialized form. You can generate it automatically with `suprnova generate-types` if your `SessionPayload` is exposed via a `#[derive(InertiaProps)]` wrapper, or define it manually as shown.

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

The `action_kind` field is a provider-specific string. Currently `"stripe_3ds"` is the only value produced by the shipped Stripe adapter. When additional adapters require client actions, they will add their own `action_kind` values following the same pattern.
