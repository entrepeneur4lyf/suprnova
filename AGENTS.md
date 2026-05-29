# AGENTS.md

This file provides guidance to Codex (Codex.ai/code) when working with code in this repository.

## Working agreement

**We only do full implementations, well tested and production ready.**

No deferring. No "we can do that later." No partial scaffolds with TODOs sprinkled
in. If a feature, test, edge case, or polish item is needed for the work to be
production-ready, it gets done now as part of the same change. If something
genuinely doesn't belong in scope, say so clearly — don't punt to a later phase.

## Project

This repository is the source of **Suprnova**, a Laravel-inspired web framework
for Rust. It is a fork of [Kit](https://github.com/dayemsiddiqui/kit) (MIT,
© Dayem Siddiqui) that we have rebranded and taken in our own direction. The
relationship with upstream Kit is severed — we do not track or contribute back.

The framework is the product. `app/` is a working example app used as a test
bed for the framework's features; it is not the goal of this repo. A separate
real consumer (nation-x.com, a social network for independent musicians) will
be built on Suprnova later, after the framework is ready.

## Design principles

1. **Parity comes from the Laravel changelog.** As Laravel ships features in
   future releases, we track that changelog and add equivalent features to
   Suprnova. Today's baseline target is Laravel 13.x — see `docs/parity/` for
   the current per-domain gap analysis.

2. **We diverge intentionally where Rust makes things better.** Laravel makes
   choices that are right for PHP and wrong for modern systems. The big one
   is concurrency — PHP's request-per-process model limits what apps can do.
   Suprnova is built on Tokio; long-lived connections, background workers,
   and concurrent IO are first-class.

3. **No gatekeeping.** When Laravel restricts a feature to a single backend
   (e.g. vectors only via Postgres `pgvector`), Suprnova supports the real
   tools for the job. Vector DBs (Qdrant, Weaviate, Milvus, LanceDB) and
   graph DBs (Neo4j, ArangoDB, SurrealDB) are first-class targets when we
   build that surface, not afterthoughts behind a Postgres-only adapter.

4. **Suprnova is the API surface; SeaORM, hyper, tokio, etc. are
   implementation details.** Consumers depend on `suprnova::*`. We re-export
   what they need and hide what they don't.

## Workspace layout

Cargo workspace at the repo root. Members:

- `framework/` — the `suprnova` crate (the framework library)
- `suprnova-cli/` — the `suprnova` binary (project scaffolder, dev server, generators, migrations)
- `suprnova-macros/` — proc macros (`#[handler]`, `#[workflow]`, `routes!`, etc.)
- `app/` — example application; used to dogfood every framework change

Plus, outside the workspace:

- `docs/` — Suprnova's user-facing docs (converted from upstream Kit's Mintlify
  source; we own them now)
- `docs/parity/` — Laravel-13 vs Suprnova gap analysis, per-domain. The master
  index is `docs/parity/README.md`. New domain analyses follow `_template.md`.
- `reference/` — read-only vendored sources for cross-referencing while
  building the framework: `framework-13.9.0/` (Laravel framework source),
  `laravel-docs-13.x/` (Laravel docs), `inertia-3.1.1/` (Inertia client
  adapters), `inertiajs-docs/` (Inertia v3 docs), `features/` (preserved
  nation-x.com app docs for later). Do not edit anything under `reference/`.

## Commands

From the repo root:

- `cargo check --workspace` — typecheck everything
- `cargo test --workspace` — run all tests
- `cargo test -p suprnova path::to::test_name` — single test in a specific crate
- `cargo clippy --workspace -- -D warnings` — lint with warnings-as-errors

The CLI binary `suprnova` is built from `suprnova-cli`. After `cargo build`,
it lives at `target/debug/suprnova`. Once installed (`cargo install --path
suprnova-cli`), end users invoke:

- `suprnova new <name> --frontend <svelte|react|vue>` — scaffold a new project
- `suprnova serve` — run backend + Vite dev server
- `suprnova migrate` / `migrate:rollback` / `migrate:status` / `migrate:fresh`
- `suprnova db:sync` — run migrations and regenerate SeaORM entities
- `suprnova generate-types` — emit TypeScript types from `#[derive(InertiaProps)]`
- `suprnova make:controller|middleware|action|error|inertia|migration|task <Name>`

## Frontend story

Suprnova bridges Rust (server) and SPA (client) via [Inertia.js](https://inertiajs.com/) 3.1.1.
Three first-class starters, all on Inertia 3 + Vite 6 + Tailwind v4:

- **Svelte 5** (runes-on) — default, our migration target
- **React 19** — `createRoot` / `hydrateRoot`
- **Vue 3.5** — `<script setup lang="ts">`

Template sources live under `suprnova-cli/src/templates/files/frontend/<framework>/`.
The CLI's `templates::scaffold_frontend(...)` is the single dispatch point that
owns frontend file writing — to add a fourth framework, mirror the directory and
extend `Frontend` + `scaffold_frontend` in `suprnova-cli/src/templates/mod.rs`.

## Database support

Three first-class drivers via SeaORM features: `sqlx-mysql`, `sqlx-postgres`,
`sqlx-sqlite`. URL detection in `framework/src/database/config.rs:database_type()`.
Connection setup in `framework/src/database/connection.rs`.

When extending the database layer (vector, graph, time-series, blob, queue
backends), do not box features behind a single backend. Drivers register
themselves; the consumer picks via env or programmatic config.

## Code style

- Re-export from `suprnova::` at the crate root anything we expect consumers
  to use. Internal modules stay `pub(crate)`.
- The `kit::` namespace is gone. All `use kit::*` was rewritten to `use
  suprnova::*` during the fork. Don't re-introduce it.
- Macros live in `suprnova-macros/`. The framework re-exports them.
- Tests live next to their code via `#[cfg(test)] mod tests`. Integration
  tests in `framework/tests/`.

<!-- gitnexus:start -->
# GitNexus — Code Intelligence

This project is indexed by GitNexus as **suprnova** (14410 symbols, 28965 relationships, 240 execution flows). Use the GitNexus MCP tools to understand code, assess impact, and navigate safely.

> If any GitNexus tool warns the index is stale, run `npx gitnexus analyze` in terminal first.

## Always Do

- **MUST run impact analysis before editing any symbol.** Before modifying a function, class, or method, run `gitnexus_impact({target: "symbolName", direction: "upstream"})` and report the blast radius (direct callers, affected processes, risk level) to the user.
- **MUST run `gitnexus_detect_changes()` before committing** to verify your changes only affect expected symbols and execution flows.
- **MUST warn the user** if impact analysis returns HIGH or CRITICAL risk before proceeding with edits.
- When exploring unfamiliar code, use `gitnexus_query({query: "concept"})` to find execution flows instead of grepping. It returns process-grouped results ranked by relevance.
- When you need full context on a specific symbol — callers, callees, which execution flows it participates in — use `gitnexus_context({name: "symbolName"})`.

## Never Do

- NEVER edit a function, class, or method without first running `gitnexus_impact` on it.
- NEVER ignore HIGH or CRITICAL risk warnings from impact analysis.
- NEVER rename symbols with find-and-replace — use `gitnexus_rename` which understands the call graph.
- NEVER commit changes without running `gitnexus_detect_changes()` to check affected scope.

## Resources

| Resource | Use for |
|----------|---------|
| `gitnexus://repo/suprnova/context` | Codebase overview, check index freshness |
| `gitnexus://repo/suprnova/clusters` | All functional areas |
| `gitnexus://repo/suprnova/processes` | All execution flows |
| `gitnexus://repo/suprnova/process/{name}` | Step-by-step execution trace |

## CLI

| Task | Read this skill file |
|------|---------------------|
| Understand architecture / "How does X work?" | `.claude/skills/gitnexus/gitnexus-exploring/SKILL.md` |
| Blast radius / "What breaks if I change X?" | `.claude/skills/gitnexus/gitnexus-impact-analysis/SKILL.md` |
| Trace bugs / "Why is X failing?" | `.claude/skills/gitnexus/gitnexus-debugging/SKILL.md` |
| Rename / extract / split / refactor | `.claude/skills/gitnexus/gitnexus-refactoring/SKILL.md` |
| Tools, resources, schema reference | `.claude/skills/gitnexus/gitnexus-guide/SKILL.md` |
| Index, status, clean, wiki CLI commands | `.claude/skills/gitnexus/gitnexus-cli/SKILL.md` |

<!-- gitnexus:end -->
