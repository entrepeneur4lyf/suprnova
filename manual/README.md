# Suprnova Manual

The complete user manual for [Suprnova](https://github.com/entrepeneur4lyf/suprnova),
a Laravel-inspired web framework for Rust.

**Start here:** [`documentation.md`](documentation.md) — the master table of
contents organised the way Laravel's docs are, so if you know your way around
those, you'll feel at home.

---

## I want to…

| Goal | Read this |
|---|---|
| **Understand what Suprnova is** | [Introduction](introduction.md) |
| **Migrate from Laravel** | [From Laravel](from-laravel.md) |
| **Compare to Axum / Actix / Rocket** | [From Rust Web](from-rust-web.md) |
| **Install and scaffold a project** | [Installation](installation.md) |
| **Build a small app end-to-end** | [Quickstart](quickstart.md) |
| **Understand how a request flows** | [Request Lifecycle](lifecycle.md) |
| **Find an API I remember from Laravel** | [Laravel Parity Map](parity.md) |
| **Look up an env var** | [Environment Variables](env-vars.md) |
| **Deploy to production** | [Deployment](deployment.md) |

## Two reading paths

The manual has two intro tracks. Pick the one that matches your background;
both converge on the shared chapters from "The Basics" onward.

- **[From Laravel →](from-laravel.md)** for PHP developers used to
  `routes`, `Eloquent`, `Auth::user()`, `php artisan`, `Mailable`, queues,
  and Blade. We translate every habit you have into the Suprnova equivalent.
- **[From Rust Web →](from-rust-web.md)** for Rust developers familiar with
  Axum, Actix, or Rocket who want a full-stack productivity layer with
  codegen, an Eloquent-style ORM, and an Inertia bridge to the frontend.

## How this manual is organised

`documentation.md` mirrors the Laravel docs index: a single master table of
contents grouped into Prologue, Getting Started, Architecture Concepts, The
Basics, Digging Deeper, Security, Database, Eloquent ORM, Testing, Payments,
Frontend, CLI, Deployment, Tutorials, and Reference. Every chapter is one
markdown file at the root of `manual/`. Files render directly on github.com —
no build step, no SaaS dependency.

## A note on completeness

Every public subsystem in `framework/src/` has a chapter. Every chapter is
validated against the actual code at the HEAD it was written against. Where
we differ from Laravel — usually because Rust gives us something better
(concurrency primitives, type-safe macros, async-everywhere) — the chapter
calls it out explicitly with a **"Why Suprnova diverges"** callout. We do
not silently break Laravel parity without saying so.

If a chapter says an API exists and you can't find it, that's a bug — please
[open an issue](https://github.com/entrepeneur4lyf/suprnova/issues).
