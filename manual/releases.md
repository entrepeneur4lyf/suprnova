# Release Notes

Suprnova is distributed through git. Nothing is published to crates.io
and there are no release tags: generated apps depend on the GitHub
repository, and the CLI installs from GitHub. See the
[CHANGELOG](../CHANGELOG.md) for the full per-version history.

## v0.1.0 - 2026-06-10

Initial Suprnova release: a Laravel-inspired full-stack web framework
for Rust with Laravel 13.x as the parity target and Tokio as the runtime
foundation.

### Install

```bash
cargo install --git https://github.com/entrepeneur4lyf/suprnova.git suprnova-cli
suprnova new myapp --frontend svelte
cd myapp
suprnova serve
```

Generated apps depend on the framework with:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

To update an app that already depends on Suprnova:

```bash
cargo update -p suprnova
```

To pin an app to a specific released commit:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", rev = "11ec6599e3c9ffc113d26c30f118f1899e439698" }
```

### What shipped

- **Laravel 13.x surface** across the core framework domains: routing,
  middleware, controllers, requests, responses, validation, errors,
  logging, container, bootstrap, sessions, database, Eloquent-style ORM,
  queues, console, broadcasting, cache, events, filesystem, HTTP client,
  mail, notifications, rate limiting, scheduling, authentication,
  authorization, and auth flows.
- **Frontend starters** for Svelte 5, React 19, and Vue 3.5 on Inertia
  3.1.1, Vite, and Tailwind.
- **Auth flows** for email verification, password reset, remember-me
  cookies, 2FA TOTP, brute-force protection, login throttling, and
  provider-backed user lookup.
- **Payments adapters** for Stripe and Paddle, plus provider-agnostic
  checkout, payment, subscription, customer-store, and webhook traits.
- **Vector backends** for memory, Qdrant, Pinecone, and MariaDB native
  `VECTOR(N)`.
- **Mail providers** for SMTP, SES, Mailgun, Postmark, SendGrid,
  Resend, plus log and in-memory drivers for local development and
  tests.
- **Broadcasting and WebSockets** with public, private, and presence
  channels, plus an opt-in sea-streamer fanout adapter.
- **Release hardening**: local release gate, enforced pre-push hook,
  rustdoc warning gate, comprehensive changelog, adapter READMEs, and
  scaffold drift guards.

See [CHANGELOG.md](../CHANGELOG.md) for the complete release notes.

## Versioning policy

Suprnova follows Cargo's SemVer interpretation:

- **MAJOR** version (`1.0.0` to `2.0.0`) means breaking API changes that
  consumers must address.
- **MINOR** version (`0.1.0` to `0.2.0`) means backwards-compatible
  feature additions where possible; during the `0.x` series, Cargo
  treats minor bumps as potentially breaking.
- **PATCH** version (`0.1.0` to `0.1.1`) means backwards-compatible bug
  fixes.

During the `0.x` line, internal API churn is expected while the
framework is dogfooded by real consumer apps.
