# Scheduling Commands

CLI surface for the per-minute task scheduler. The three `schedule:*`
subcommands all delegate into your application binary's `Application::run()`
dispatch, so they see the same config, services, observers, and listeners
that a request handler does. The full scheduler model — `Task` trait, fluent
cron API, `without_overlapping`, `run_in_background` — lives in
[Scheduling](scheduling.md); this chapter is the operator reference for the
commands themselves.

## How the commands run

`suprnova schedule:run`, `suprnova schedule:work`, and `suprnova schedule:list`
are thin shells that invoke `cargo run -- schedule:<subcommand>` against the
project in the current directory. The same subcommands are also reachable
directly on the application binary in production:

```bash
# In development (from the project root, source build):
suprnova schedule:run

# In production (binary on PATH):
/usr/local/bin/myapp schedule:run
```

The runtime drivers (Cache, Queue, RateLimit, Mail) and your
`bootstrap_fn` are booted before any task runs, so a scheduled task can
resolve services from the container exactly like a controller — see
[Application Bootstrap](bootstrap.md).

You must wire the scheduler into the application builder for the
subcommands to find any tasks:

```rust
// cmd/main.rs (backend starter) or src/main.rs (API starter)
Application::new()
    .config(my_app::config::register)
    .bootstrap(my_app::bootstrap::bootstrap)
    .routes(my_app::routes::register)
    .schedule(my_app::schedule::register)   // <-- the scheduler hook
    .migrations::<my_app::migrations::Migrator>()
    .run()
    .await
```

`suprnova make:task <Name>` wires this automatically; if you build the
chain by hand, add the `.schedule(...)` call yourself.

## schedule:run

Evaluate every registered task once and run the ones whose cron expression
matches the current minute. Designed to be invoked by system cron every
minute. Exits non-zero if any task failed; exits zero (with `No tasks were
due.`) if nothing was due this minute.

```bash
suprnova schedule:run
```

### Example output

```
Running due scheduled tasks...
  ✓ cleanup:logs
  ✓ send:reminders
```

When a task returns an error, its line is prefixed with `✗` and the error
message is appended:

```
Running due scheduled tasks...
  ✓ cleanup:logs
  ✗ backup:database: connection refused
```

When no task is due this minute:

```
Running due scheduled tasks...
No tasks were due.
```

### Crontab entry

A single entry runs the scheduler every minute. The application binary
evaluates all due tasks itself, so this is the only crontab line a
production host needs:

```cron
* * * * * cd /path/to/your/project && /usr/local/bin/myapp schedule:run >> /var/log/myapp/schedule.log 2>&1
```

If you're running `schedule:run` from system cron on more than one host
(or alongside a `schedule:work` daemon), tasks marked
`.without_overlapping()` need a configured Cache backend
(`CACHE_DRIVER=redis` is the production-grade choice) to coordinate
across processes — see [Preventing overlap](scheduling.md#preventing-overlapping)
for the lock semantics.

## schedule:work

Run the scheduler as a long-lived daemon. The first tick is aligned to the
next minute boundary, then the loop evaluates due tasks once per minute
until it receives `SIGINT` (Ctrl-C) or `SIGTERM`. On shutdown, any
`run_in_background` tasks still in flight are awaited before exit so they
don't get torn down mid-write.

```bash
suprnova schedule:work
```

### Example output

```
Starting scheduler daemon...
Press Ctrl+C to stop

==============================================
  suprnova Scheduler Daemon
==============================================
  3 task(s) registered. Press Ctrl+C to stop.
==============================================
```

Each tick is quiet — only failures are logged. On shutdown:

```
suprnova: scheduler shutting down.
suprnova: waiting for 1 background task(s) to finish…

Scheduler daemon stopped.
```

### Use cases

- **Development.** No crontab required — start the daemon in a terminal
  and watch it tick.
- **Docker.** Use as the container's main process when you want one image
  to play the scheduler role.
- **Systemd.** Manage it as a long-running unit (see [systemd unit](#systemd-unit)
  below).

### systemd unit

```ini
# /etc/systemd/system/myapp-scheduler.service
[Unit]
Description=MyApp Scheduler
After=network.target

[Service]
Type=simple
User=www-data
WorkingDirectory=/path/to/your/project
ExecStart=/usr/local/bin/myapp schedule:work
Restart=always
RestartSec=5

[Install]
WantedBy=multi-user.target
```

```bash
sudo systemctl daemon-reload
sudo systemctl enable myapp-scheduler
sudo systemctl start myapp-scheduler
```

`Restart=always` brings the daemon back up if it crashes; `RestartSec=5`
debounces a crash loop. Because the framework's panic boundary catches
panicking tasks and converts them to `FrameworkError`, a single bad task
should not crash the daemon — `Restart=always` is for the rare process-wide
failure (OOM, parent kill).

## schedule:list

Print every registered task with its cron expression and description.

```bash
suprnova schedule:list
```

### Example output

```
Registered scheduled tasks:
  cleanup:logs [0 3 * * *] — Removes logs older than 30 days
  send:reminders [0 9 * * *] — Sends daily reminder emails
  backup:database [0 0 * * 0] — Weekly database backup
  heartbeat [* * * * *]
```

Tasks with a `.description(...)` chained on the builder include the
description after the cron expression; tasks without a description show
only the cron.

When nothing is registered (the `.schedule(...)` builder call is missing,
or `schedule::register` is a no-op):

```
No scheduled tasks registered.
Define tasks in src/schedule.rs and wire it with `Application::schedule(schedule::register)`.
```

## Generating a task

The framework ships a generator that creates the task, wires it into the
project, and adds the scheduler call to your `main.rs`:

```bash
suprnova make:task CleanupLogs
```

This:

1. Creates `src/tasks/cleanup_logs_task.rs` (a working `Task` stub that
   logs its own duration)
2. Creates `src/tasks/mod.rs` (re-exporting `CleanupLogsTask`) if it
   doesn't already exist
3. Creates `src/schedule.rs` (with a `register(&mut Schedule)` function)
   if it doesn't already exist
4. Declares `pub mod schedule;` and `pub mod tasks;` in `src/lib.rs`
5. Adds `.schedule(<crate>::schedule::register)` to the `Application`
   chain in `cmd/main.rs` (or `src/main.rs` for the API starter)

Steps 2–5 are idempotent, so re-running `make:task` repairs wiring that
was removed by hand. See [Generators](cli-generators.md) for the broader
`make:*` family.

After generating, register the task in `src/schedule.rs`:

```rust
use suprnova::Schedule;
use crate::tasks::CleanupLogsTask;

pub fn register(schedule: &mut Schedule) {
    schedule.add(
        schedule.task(CleanupLogsTask::new())
            .daily()
            .at("03:00")
            .name("cleanup:logs")
            .description("Removes logs older than 30 days")
    );
}
```

The fluent builder API (`.daily()`, `.cron(...)`, `.without_overlapping()`,
`.run_in_background()`, day-specific modifiers) is fully covered in
[Scheduling](scheduling.md).

## Exit codes

| Command | Exit zero | Exit non-zero |
|---|---|---|
| `schedule:run` | every due task returned `Ok(())`, or no tasks were due | at least one task returned `Err(_)` or panicked |
| `schedule:work` | clean shutdown via `SIGINT` / `SIGTERM` (the wrapper treats exit code 130 as clean Ctrl-C) | bootstrap failure, or the daemon process aborted |
| `schedule:list` | listing succeeded (including the "no tasks registered" message) | application failed to boot |

Background-task failures inside `schedule:work` are logged to stderr but
do not exit the daemon — the `JoinSet`'s `catch_unwind` boundary surfaces
them as `FrameworkError` and the tick loop continues.

### Why Suprnova diverges

Laravel's `schedule:run` is the only first-class entry point; the daemon
form (`schedule:work`) is a backport for hosts without crontab. PHP has
no long-lived process, so each minute is a fresh runtime that has to
re-boot the framework, the container, and every service binding.

In Suprnova the daemon is first-class. `schedule:work` runs inside the
same Tokio runtime that serves HTTP, so:

- **Background tasks compose with the tick loop.** A `.run_in_background()`
  task is spawned into a `JoinSet`; the loop polls completed ones before
  the next tick and drains the rest on shutdown. Laravel spawns a child
  process per background task.
- **Graceful shutdown drains in-flight work.** Ctrl-C / SIGTERM lets
  inline tasks finish their current call and awaits every background
  spawn before exit. Laravel relies on the OS to kill the cron child.
- **Boot cost is paid once.** The container, drivers, and your
  `bootstrap_fn` boot at daemon start, not at every tick. `schedule:run`
  still pays the boot cost per invocation (it's a single-shot subcommand),
  but the daemon path is where the runtime model pays off.

`schedule:run` still works (and is the right choice when system cron is
already the operator's source of truth). Pick whichever fits your
deployment shape — both share the same task definitions.

## Next

- [Scheduling](scheduling.md) — the `Task` trait, fluent cron API,
  `without_overlapping`, `run_in_background`, and same-minute dedup
- [Generators](cli-generators.md) — the full `make:*` family, including
  `make:task`
- [Console](console.md) — `#[command]`-annotated one-shot operator
  tasks (not on a schedule)
- [Queues](queues.md) — for work that should be picked up by a worker
  rather than tick on a clock
- [Application Bootstrap](bootstrap.md) — how `.schedule(...)` plugs into
  the builder, and what tasks can resolve from the container
