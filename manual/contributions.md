# Contribution Guide

Suprnova is open-source under the MIT License. Contributions of every
size are welcome — bug reports, doc fixes, tests, features, full
subsystems. This chapter describes how to land a change cleanly.

## Where to start

- **Bugs** — file an issue at
  [github.com/entrepeneur4lyf/suprnova/issues](https://github.com/entrepeneur4lyf/suprnova/issues)
  with a reproduction. If it's reproducible from `suprnova new`, that's
  the gold standard; otherwise a minimal failing test inside
  `framework/tests/` is what we'll land the fix against.
- **Features** — open an issue describing the use case first. We may
  already have a planned shape (often the Laravel equivalent), and
  syncing early saves a rewrite.
- **Docs** — PRs straight against `manual/` are welcome. If you find a
  chapter says an API exists and you can't find it, that's a doc bug —
  PR a fix or open an issue.
- **Tests** — adding a test that pins existing behaviour is always
  welcome, no issue needed.

## Development setup

```bash
git clone https://github.com/entrepeneur4lyf/suprnova.git
cd suprnova
cargo check --workspace          # type-check everything
cargo test --workspace           # run the full suite (~3400 tests)
cargo clippy --workspace --all-targets -- -D warnings
```

The workspace has:

- `framework/` — the `suprnova` crate (the framework library)
- `suprnova-cli/` — the `suprnova` binary
- `suprnova-macros/` — proc macros
- `app/` — internal dogfood app (exercises every feature together)
- `crates/suprnova-payments-stripe/` — Stripe payments adapter
- `crates/suprnova-payments-paddle/` — Paddle payments adapter
- `crates/suprnova-web-push/` — Web Push transport
- `manual/` — this user manual (plain markdown)

For all-in-one workspace commands:

```bash
cargo check --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all
```

For one crate:

```bash
cargo test -p suprnova
cargo clippy -p suprnova-cli --tests -- -D warnings
```

## House rules

Three rules survive every PR review:

**1. Full implementations only.** No TODO comments, no partial scaffolds,
no "we'll add tests later". A bug fix lands with the regression test
that pins it. A new feature lands with the tests that cover the happy
path, the obvious edge case, and at least one failure mode.

**2. Match the existing style.** `cargo fmt` is canonical. Clippy under
`-D warnings` is canonical. If your IDE adds a different blank-line
convention, override your IDE. We don't write style debates into PR
comments.

**3. Public-surface code returns `Result`, doesn't panic.** The panic
boundary in `execute_chain_safely` is a safety net for genuine bugs —
not licence to `.unwrap()` in library code. Where a Laravel-style
infallible name (e.g. `register_route_name`) ships, pair it with a
`try_*` Result sibling.

See [Error Model](error-model.md) for the full error contract and
[Lock Policy](lock-policy.md) for the poisoned-lock conventions.

## Style guide for code

- **Imports** — use crate-root re-exports (`use suprnova::*;`); avoid
  importing from internal modules
- **Re-exports** — anything a consumer names goes into
  `framework/src/lib.rs` re-exports. Internal helpers stay
  `pub(crate)`.
- **Doc comments** — explain *why* in the code; *what* is what the
  code says. Don't restate signatures in docstrings.
- **Lifetimes** — most public APIs hide them. If your signature
  exposes a non-obvious lifetime, the API probably needs reshape.
- **`unsafe`** — avoid. We have none in the framework today and want
  to keep it that way.
- **Macros** — proc macros live in `suprnova-macros/`. Inherent impls
  can't shadow trait defaults through trait dispatch (see
  `framework/CLAUDE.md` for the trap); emit trait method overrides
  instead.

## Style guide for the manual

This chapter is itself part of the manual, so follow the same shape:

- One markdown file per chapter at `manual/<chapter>.md`
- No frontmatter, no Mintlify components — plain GitHub-rendered markdown
- Cross-link with flat `(other-chapter.md)` paths (same directory)
- Open with a one-paragraph intro: what + when
- H2 for top-level, H3 for subsections
- Every chapter has at least one runnable code example
- Where Suprnova diverges from Laravel, add a `### Why Suprnova diverges`
  callout
- Close with a `## Next` section listing 3-5 related chapters

Don't sell; explain.

## PR shape

We prefer small, focused PRs that do one thing. Bundled PRs work when
the bundle is internally consistent (e.g. a single subsystem
hardening sweep), but two unrelated changes belong in two PRs.

A good PR description includes:

1. **What** — one sentence
2. **Why** — the motivating issue, use case, or audit finding
3. **How** — the approach in a paragraph
4. **Tests** — what's covered, what was hard to test and why

The maintainers will run `cargo check / test / clippy` before
landing; you can save us time by confirming they pass locally first.

## Adding a new subsystem

If you're adding a substantial feature (a new driver, a new
subsystem), the existing chapter on
[Application Bootstrap](bootstrap.md) and the
[Service Container](container.md) sections of the manual describe
the integration points. The high-level checklist:

1. **Crate-root re-export** — add the public API names to
   `framework/src/lib.rs`
2. **Trait-driven if it has drivers** — define a trait, ship the
   default driver in-tree, register others via inventory or
   `App::bind`
3. **No gatekeeping by backend** — design principle #3: if a feature
   has multiple plausible backends (DB, cache, queue, vector store,
   payments), don't lock to one
4. **Tests** — both unit tests next to the code and an integration
   test in `framework/tests/` driving `handle_request` end-to-end if
   the surface is HTTP-facing
5. **Manual chapter** — add `manual/<your-subsystem>.md` and link it
   from `manual/documentation.md`

## Security

Report security issues privately to
**shawn.payments@gmail.com** (the project maintainer). We'll
acknowledge within a few days, work the fix on a private branch, and
coordinate disclosure with you.

Do not file security issues as public GitHub issues until a fix has
shipped.

## License

MIT, with attribution to the upstream
[Kit project](https://github.com/dayemsiddiqui/kit) we forked from.
By contributing you agree your contribution lands under the same
license.
