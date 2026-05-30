# Release Notes

Suprnova is **pre-launch** — `v0.1.0` is not yet tagged. This page
will host the full release history once the first version ships.

## What's in the current `main` branch

As of the latest commit on `main`:

- **Laravel-13 parity sweep** is complete across all 30 module groups
  (HTTP, routing, controllers, requests, responses, middleware, CSRF,
  CORS, validation, errors, logging, container, bootstrap, sessions,
  database, Eloquent, queues, console, broadcasting, cache, events,
  filesystem, HTTP client, mail, notifications, rate limiting,
  scheduling, authentication, auth flows, authorization)
- **External audit backlog** is fully drained: 0 HIGH, 0 MEDIUM, 0 LOW
  findings open
- **Test suite** runs 3400+ tests across the workspace, gate-clean
- **Frontend starters** ship for Svelte 5 (default), React 19, and Vue 3.5
- **Payments adapters** in tree for Stripe (gateway model) and Paddle
  (Merchant of Record model)
- **Vector backends** in tree for Memory, Qdrant, Pinecone, and
  MariaDB
- **Mail providers** in tree for SMTP, SES, Mailgun, Postmark, SendGrid,
  Resend, plus log and in-memory drivers for tests

## What's gating `v0.1.0`

The framework's API surface is considered settled — internal API churn
this cycle is fixing audit findings and adding Laravel-parity features
that were already specified. What's left before tagging:

- **CI re-enable** — the GitHub Actions workflows are stubbed; the
  cross-platform runners (Linux + macOS via Tart on Apple hardware)
  need to be activated
- **`docs/deployment.md`** — production deployment guide (Railway,
  Digital Ocean, Hetzner)
- **First release tag** — once CI is green
- **Scaffold edition bump** — generated `Cargo.toml` emits
  `edition = "2021"` but the workspace uses 2024; needs alignment
- **Payments adapter distribution** — `suprnova-payments-stripe` and
  `suprnova-payments-paddle` face the same "framework not on
  crates.io" constraint as the CLI; decide git-dep vs eventual publish
  before tagging

## Versioning policy

After `v0.1.0`, Suprnova follows Cargo's SemVer interpretation:

- **MAJOR** version (`1.0.0` → `2.0.0`) — breaking API changes that
  consumers must address
- **MINOR** version (`0.1.0` → `0.2.0`) — backwards-compatible feature
  additions; consumers can upgrade freely
- **PATCH** version (`0.1.0` → `0.1.1`) — backwards-compatible bug
  fixes

During the `0.x` series Cargo treats minor version bumps as
potentially breaking by default. We'll signal compat intent in each
minor release's notes.

## Update path before `v0.1.0`

While we're pre-launch and depending via git:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git" }
```

To pull the latest:

```bash
cargo update -p suprnova
```

If you want to pin to a specific commit:

```toml
suprnova = { git = "https://github.com/entrepeneur4lyf/suprnova.git", rev = "abc123def" }
```

Once `v0.1.0` ships and Suprnova is on crates.io, the install story
will simplify to `cargo install suprnova-cli` and
`suprnova = "0.1"` in your `Cargo.toml`.

## When `v0.1.0` ships

This page will be updated with:

- The release date
- A complete CHANGELOG-style summary of what's in `v0.1.0`
- The download/install commands
- Migration notes from `main` snapshots (likely minimal — APIs are
  settled)

Watch the [GitHub releases](https://github.com/entrepeneur4lyf/suprnova/releases)
page for the announcement.
