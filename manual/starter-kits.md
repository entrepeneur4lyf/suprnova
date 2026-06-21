# Starter Kits

Starter kits are ready-made Suprnova applications you fork and ship. Each one
wires the controllers, routes, migrations, frontend pages, and tests for a
complete product surface — so you start from a running app, not an empty
scaffold.

Two kits ship today, modelled on Laravel's lineage. Pick the one closest to
what you're building and customise from there.

## Nebula — authentication (Breeze-tier)

**Repo: [github.com/entrepeneur4lyf/Nebula](https://github.com/entrepeneur4lyf/Nebula)**

The minimal full-auth kit — Suprnova's Breeze equivalent. Everything you need
for accounts and nothing you don't:

- Registration with email verification
- Login with remember-me
- Password reset with anti-enumeration responses
- Profile management — update email and password, delete account
- A branded Inertia 3 + Svelte 5 frontend (dark by default), with the
  logged-in user menu wired

Nebula ships two test suites: facade-level auth logic, and a wire-level HTTP
suite that drives real routes, sessions, CSRF round-trips, and the
guest / auth / verified gates over a loopback socket.

Reach for Nebula when you want a clean account-management foundation to build
your own product on top of.

## Pulsar — product site & community

**Repo: [github.com/entrepeneur4lyf/Pulsar](https://github.com/entrepeneur4lyf/Pulsar)**

A complete developer-tool / SaaS company site on Vue 3.5 + Vuetify. Everything
in Nebula's auth story, plus the surfaces a real product site needs:

- Marketing landing page and a user dashboard
- A Markdown documentation pipeline (`docs:build`) with search and a generated
  table of contents
- A blog / articles system with an RSS feed
- Public member profiles
- Taxonomy — topics, tags, and categories
- Role-based access control: roles, permissions, and gates
- Admin and moderation surfaces for content and members

Pulsar is the source kit for downstream products such as `suprnova.app`. Reach
for it when you're shipping a product site with docs, a blog, and a member
community — not just authentication.

## Which kit?

| You want… | Start with |
|---|---|
| Accounts and a place to build | **Nebula** |
| A full product site — landing, docs, blog, community, RBAC | **Pulsar** |
| An API-only backend (token auth, no frontend) | `suprnova new my-api --api` |

Both kits track the framework as a git dependency and run on the same stack you
already know — see each repo's README for setup. More kits are planned — see
the [roadmap](../ROADMAP.md).

## What the default scaffold gives you

If neither kit fits, `suprnova new my-app --frontend svelte` (or `react`, or
`vue`) already ships a working authentication flow — login, register, logout,
session authentication with the `authenticate` middleware, CSRF protection, and
a protected `/dashboard` route — on any of the three frontends (Svelte 5,
React 19, Vue 3.5) with Tailwind v4 and Inertia v3. See
[Installation](installation.md) for the scaffold output and
[Quickstart](quickstart.md) for the first-five-minutes walkthrough.

For API-only services, `suprnova new my-api --api` ships the same backend stack
with token-based auth instead of sessions, and no frontend.

## Contributing a starter kit

Built something reusable on top of Suprnova and want to upstream it as a
canonical kit? See [Contributions](contributions.md). We're happy to take a
real implementation and round it into a generic kit.
