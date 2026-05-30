# Starter Kits

This chapter will document Suprnova starter kits — Breeze/Jetstream/
Spark-tier scaffolds that ship pre-wired authentication UI, billing,
teams, and other higher-level features on top of a plain `suprnova
new` scaffold.

**Status: planned, not yet shipped.** Starter kits ship after `v0.1.0`.

## What the default scaffold gives you today

`suprnova new my-app --frontend svelte` (or `react`, or `vue`) already
ships a working authentication flow:

- Login, register, logout endpoints
- Session-based authentication with the `authenticate` middleware
- CSRF protection
- A protected `/dashboard` route demonstrating the flow
- Three frontend variants (Svelte 5, React 19, Vue 3.5) with
  Tailwind v4 and Inertia v3

That base is enough to ship many apps. See
[Installation](installation.md) for the scaffold output and
[Quickstart](quickstart.md) for the first-five-minutes walkthrough.

For API-only services, `suprnova new my-api --api` ships the same
backend stack with token-based auth instead of sessions, and no
frontend.

## What's coming

Three tiers, modelled on Laravel's lineage:

### Breeze-tier (free, planned)

The minimum onboarding kit. Adds to the default scaffold:

- Email verification flow with a verification controller and mail
  template
- Password reset flow with the reset controller and mail template
- Profile page with email/password update and account deletion
- Logged-in user dropdown menu with profile/logout links

Most of these subsystems already exist in the framework
([Auth Flows](auth-flows.md), [Mail](mail.md), [Notifications](notifications.md)) —
the starter kit will wire the controllers, routes, and frontend pages
together so you start with a complete account-management story
instead of writing it yourself.

### Jetstream-tier (free, planned)

Adds team-based account management on top of Breeze-tier:

- Two-factor authentication enrollment flow (the
  [Auth Flows](auth-flows.md) TOTP surface, exposed in the UI)
- API token management (personal access tokens)
- Team creation, membership, role-based authorization via the
  [Authorization](authorization.md) subsystem
- Team invitations via [Notifications](notifications.md)

### Spark-tier (planned)

Adds billing and subscriptions on top of Jetstream-tier, using the
[Payments](payments.md) surface:

- Pricing plans configurable in `config/`
- Stripe ([Payments: Stripe](payments-stripe.md)) or Paddle
  ([Payments: Paddle](payments-paddle.md)) wired to a billing page
- Subscription management UI (upgrade, downgrade, cancel)
- Invoice history with the Inertia checkout flow

Spark-tier depends on Payments — which has shipped. The remaining
work is the kit-level wiring and frontend pages.

## When

After `v0.1.0`. Watch [Release Notes](releases.md) for the
announcement; we'll add chapters for each kit as it ships, with full
scaffolder integration (`suprnova new my-app --kit breeze`).

## Roll your own in the meantime

Everything the planned kits will do is buildable today from the
existing subsystems. If you want a Breeze-equivalent now:

1. Start with the default scaffold (`suprnova new`)
2. Add email verification per [Auth Flows](auth-flows.md)
3. Add password reset per [Auth Flows](auth-flows.md)
4. Add a profile controller + Inertia page per
   [Controllers](controllers.md) and [Frontend](frontend.md)
5. Wire mail templates per [Mail](mail.md)

Same for Jetstream (add teams + 2FA UI) and Spark (add Stripe/Paddle
checkout pages). The kits will collapse this into one scaffolder
flag; until they ship, the manual chapters above are the recipe.

## Contributing a starter kit

If you build one of these on top of a real product and want to
upstream it as the canonical kit, see
[Contributions](contributions.md). We're happy to take an
implementation as a starting point and round it into a generic kit.
