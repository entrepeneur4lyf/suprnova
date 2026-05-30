---
title: CSRF Protection
description: Cross-Site Request Forgery protection in suprnova
icon: shield-check
---

suprnova provides automatic CSRF (Cross-Site Request Forgery) protection for all state-changing requests. This prevents malicious websites from executing actions on behalf of authenticated users.

## Overview

CSRF protection in suprnova:

- **Automatically validates** all POST, PUT, PATCH, and DELETE requests
- **Generates per-session tokens** for maximum security
- **Integrates with Inertia.js** via meta tag and axios interceptor
- **Uses constant-time comparison** to prevent timing attacks

## How It Works

1. When a session is created, suprnova generates a cryptographically secure CSRF token
2. This token is embedded in the HTML via a `<meta>` tag
3. The frontend automatically includes the token in request headers
4. suprnova validates the token on every state-changing request
5. Invalid tokens result in a 419 "Page Expired" response

## Frontend Integration

### Automatic Setup

suprnova projects are pre-configured with CSRF protection. The generated `main.tsx` sets up axios to automatically include the CSRF token:

```tsx
// frontend/src/main.tsx
import axios from 'axios';

// Get CSRF token from meta tag
const token = document.querySelector('meta[name="csrf-token"]')?.getAttribute('content');
if (token) {
  axios.defaults.headers.common['X-CSRF-TOKEN'] = token;
}
```

### Inertia Forms

When using Inertia's `useForm` hook, CSRF tokens are automatically included:

```tsx
import { useForm } from '@inertiajs/react';

function CreatePost() {
  const { data, setData, post } = useForm({
    title: '',
    content: '',
  });

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    post('/posts');  // CSRF token automatically included
  };

  return (
    <form onSubmit={handleSubmit}>
      {/* ... */}
    </form>
  );
}
```

### Manual Requests

For manual fetch or axios requests, include the token from the meta tag:

```tsx
const csrfToken = document.querySelector('meta[name="csrf-token"]')?.getAttribute('content');

// Using fetch
fetch('/api/data', {
  method: 'POST',
  headers: {
    'Content-Type': 'application/json',
    'X-CSRF-TOKEN': csrfToken || '',
  },
  body: JSON.stringify({ /* ... */ }),
});

// Using axios (already configured by default)
axios.post('/api/data', { /* ... */ });
```

## Backend Configuration

CSRF middleware is automatically registered in `bootstrap.rs`:

```rust
use suprnova::{Router, SessionMiddleware, CsrfMiddleware};

pub fn boot(router: Router) -> Router {
    router
        // Session middleware must come first
        .middleware(SessionMiddleware::new(session_store, session_config))
        // CSRF protection
        .middleware(CsrfMiddleware::new())
}
```

## Excluding Routes

Some routes may need to bypass CSRF protection (e.g., webhook endpoints). You can exclude specific routes:

```rust
use suprnova::CsrfMiddleware;

// In your bootstrap or route configuration
let csrf = CsrfMiddleware::new()
    .except(vec!["/webhooks/stripe", "/webhooks/github"]);
```

Each pattern is a Laravel-style glob (`*` matches any run of
characters):

- exact paths: `"/login"` matches only `/login`
- trailing wildcards: `"/webhooks/*"` matches `/webhooks/stripe`,
  `/webhooks/github/events`, …
- mid-pattern wildcards: `"/api/*/internal"` matches `/api/v1/internal`
  and `/api/v2/internal`
- leading wildcards: `"*/healthz"` matches any path ending in `/healthz`

Patterns are normalized — a leading slash is optional, so
`"webhooks/*"` and `"/webhooks/*"` behave identically.

If you need an exemption to fire on **one** HTTP verb only, use
`except_method`:

```rust
use suprnova::CsrfMiddleware;

// Stripe POST callbacks are exempt; DELETEs against the same prefix
// still require a token.
let csrf = CsrfMiddleware::new()
    .except_method("POST", "/webhooks/stripe/*");
```

## CSRF Helper Functions

suprnova provides helper functions for working with CSRF tokens:

```rust
use suprnova::csrf::{csrf_token, csrf_meta_tag};

// Get the current CSRF token
if let Some(token) = csrf_token() {
    println!("Token: {}", token);
}

// Generate a meta tag (used internally by Inertia)
let meta = csrf_meta_tag();
// Returns: <meta name="csrf-token" content="...">
```

## Error Handling

When CSRF validation fails, suprnova returns a 419 status code with a "CSRF token mismatch" message. You can customize this behavior:

```tsx
// In your frontend error handling
axios.interceptors.response.use(
  response => response,
  error => {
    if (error.response?.status === 419) {
      // Token expired - reload the page to get a new token
      window.location.reload();
    }
    return Promise.reject(error);
  }
);
```

## Security Considerations

suprnova's CSRF implementation follows security best practices:

- **Per-session tokens**: Each session has its own unique CSRF token
- **Secure generation**: Tokens are generated using cryptographically secure random bytes
- **Constant-time comparison**: Token validation uses constant-time comparison to prevent timing attacks
- **Token regeneration**: Tokens are regenerated on logout to prevent session fixation
- **SameSite cookies**: Combined with SameSite=Lax cookies for defense in depth

## Testing

When writing tests, you'll need to include CSRF tokens. suprnova's testing utilities handle this automatically:

```rust
#[tokio::test]
async fn test_protected_route() {
    let app = test_app().await;

    // First, get a session
    let response = app.get("/login").await;
    let csrf_token = response.csrf_token();

    // Include token in POST request
    let response = app
        .post("/login")
        .header("X-CSRF-TOKEN", csrf_token)
        .json(&json!({
            "email": "test@example.com",
            "password": "password"
        }))
        .await;

    assert_eq!(response.status(), 302);
}
```

## Inertia-Specific Behavior

When using Inertia.js, CSRF handling has some special considerations:

- The CSRF token is injected into the HTML page via a `<meta>` tag
- Inertia automatically reads this token and includes it in XHR requests
- For 419 responses, Inertia can be configured to handle the redirect:

```tsx
// In your createInertiaApp setup
createInertiaApp({
  resolve: name => {/* ... */},
  setup({ el, App, props }) {
    // Handle 419 errors globally
    // ...
  },
});
```
