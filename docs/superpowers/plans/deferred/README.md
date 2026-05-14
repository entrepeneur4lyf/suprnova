# Deferred Plans

Plans in this directory were written but are **not part of v1 scope**.
They're preserved for historical context and possible future revisit.

## `2026-05-14-phase-15-browser-testing.md` — PERMANENTLY deferred

Browser testing (Dusk-style — fantoccini WebDriver bindings, page
objects, screenshot-on-fail, `suprnova dusk` runner).

**Why deferred:** Browser testing via AI agents (Claude +
`chrome-devtools-mcp`, agent-browser, or any other tool-using web agent)
covers the same ground Laravel Dusk does, without the framework needing
to ship WebDriver bindings or page-object machinery. Consumers write
browser tests as agent instructions, not as framework code.

**Status:** Closed. Do not re-introduce.

## `2026-05-14-phase-14-dev-observability.md` — Deferred to v2+

Telescope-style debug dashboard (`/telescope`) + Pulse-style perf
dashboard (`/pulse`) — recorders for requests / queries / mail / jobs /
events / exceptions, time-window aggregation, Inertia UI.

**Why deferred:** Scope management. Phase 1 (Observability foundation)
already ships logging + events + error tracing + SSE — that's enough to
power external observability tools (Sentry, Datadog, Grafana, lnav) which
most teams already run. We may come back to build first-class dashboards
once we've validated that consumers actually want them.

**Status:** On hold. Revisit after Phases 1–13 ship and we get consumer
feedback.
