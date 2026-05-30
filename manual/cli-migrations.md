# CLI Migrations

The `suprnova` developer CLI shells into your application binary to drive
SeaORM's migration runner, so the same migration set executes whether you
run it from a developer terminal, from CI, or implicitly at server startup.
Use these commands to author migration files, apply them, roll back, and
keep your generated SeaORM entities in sync with the schema.

For the schema-authoring API (column types, indexes, foreign keys, the full
`MigrationTrait`), see [Migrations](migrations.md). For inserting test
data after the schema lands, see [Seeding](seeding.md).

## make:migration

Generate a new migration file under `src/migrations/` and wire it into the
`Migrator` in `src/migrations/mod.rs`.

```bash
suprnova make:migration <name>
```

`<name>` is normalised to snake_case. The generator recognises the
standard naming patterns and uses them to pick the `DeriveIden` enum:

- `create_<table>_table` — scaffolds a `create_table` body
- `add_<column>_to_<table>` — scaffolds a stub for `alter_table`
- `drop_<table>_table` — scaffolds a `drop_table` body
- anything else — uses the name as the table identifier

### Examples

```bash
suprnova make:migration create_users_table
suprnova make:migration add_email_to_users
suprnova make:migration drop_legacy_sessions_table
```

### Generated file

The file is written to `src/migrations/m{YYYYMMDD}_{HHMMSS}_<name>.rs`
(for example `m20260530_142301_create_users_table.rs`) and added to the
`Migrator::migrations()` vec.

```rust
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(Users::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Users::Id)
                            .integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(Users::CreatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Users::UpdatedAt)
                            .timestamp()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(Users::Table).to_owned())
            .await
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
    CreatedAt,
    UpdatedAt,
}
```

Edit the generated file to declare your columns, indexes, and constraints.
See [Migrations](migrations.md) for the full schema-builder surface.

## migrate

Run every pending migration in `src/migrations/`.

```bash
suprnova migrate
```

The CLI shells out to `cargo run -- migrate` so your app's `Application`
runner does the work — same binary, same `Migrator`, same database
connection that `serve` would use.

```
Running migrations...
Migrations completed successfully!
```

The serve / web:run path auto-runs `migrate` before binding the socket
unless you opt out with `--no-migrate` or set
`SUPRNOVA_AUTO_MIGRATE_BEST_EFFORT=true` to keep going past a failure.
A migration error during auto-migrate exits non-zero before the server
boots; see `framework/src/app/mod.rs` for the fail-closed contract.

## migrate:status

Print the applied/pending state of every migration.

```bash
suprnova migrate:status
```

```
Migration status:
...SeaORM-formatted table of applied/pending migrations...
```

The body of the report comes from SeaORM's `MigratorTrait::status`, so the
exact formatting tracks the SeaORM version your app depends on.

## migrate:rollback

Roll back the last applied migration (or the last `N`).

```bash
suprnova migrate:rollback [--step <N>]
```

| Option | Default | Description |
|---|---|---|
| `--step <N>` | `1` | Number of migrations to roll back |

```bash
# Roll back one migration
suprnova migrate:rollback

# Roll back the last three
suprnova migrate:rollback --step 3
```

```
Rolling back 3 migration(s)...
Rollback completed successfully!
```

Each migration's `down()` runs in reverse application order. A failing
`down()` exits non-zero and leaves the rest of the chain untouched —
nothing further is attempted.

## migrate:fresh

Drop every table in the database and re-run every migration from scratch.

```bash
suprnova migrate:fresh
```

```
WARNING: Dropping all tables and re-running migrations...
Database refreshed successfully!
```

This destroys all data in the connected database. It is meant for local
development and test setup, not for any environment where the data
matters. There is no confirmation prompt — if `DATABASE_URL` points at
production, this will obliterate production.

## db:sync

Regenerate the SeaORM entity files in `src/models/entities/` from the
current database schema, and (when a `src/bin/migrate.rs` exists) run
pending migrations first.

```bash
suprnova db:sync [--skip-migrations] [--regenerate-models]
```

| Option | Description |
|---|---|
| `--skip-migrations` | Skip the migration pass and only regenerate entities |
| `--regenerate-models` | Overwrite `src/models/<table>.rs` files too, not just `src/models/entities/<table>.rs` |

### What it does

1. (Optional) Runs pending migrations. The default scaffold does not
   ship a `src/bin/migrate.rs`, so this step is a no-op and prints
   `Migration binary not found, skipping migrations`. In a default
   project, run `suprnova migrate` first, then `suprnova db:sync
   --skip-migrations`.
2. Connects to `DATABASE_URL`, introspects every user table (skipping
   `seaql_migrations` and any name starting with `_`), and writes one
   entity file per table to `src/models/entities/<table>.rs`.
3. Writes a thin user-facing model file at `src/models/<table>.rs` —
   but only if that file does not already exist, so your hand-written
   accessors, scopes, and observer hooks survive.
4. `--regenerate-models` overrides the protection in step 3 and
   overwrites those user files. Use it when you have not customised
   them yet, or when you have a backup.

### Typical workflow

```bash
# 1. Author a migration
suprnova make:migration create_posts_table
# (edit src/migrations/m..._create_posts_table.rs)

# 2. Apply it
suprnova migrate

# 3. Regenerate the entities so the new table is reachable from code
suprnova db:sync --skip-migrations
```

### Why Suprnova diverges

Laravel has one global `artisan` that owns every framework command,
including `db:seed`. Suprnova splits this in two:

- The `suprnova` developer CLI (this chapter) owns project scaffolding,
  generators, and the migration commands. It is installed once per
  developer machine via `cargo install` and shells into your app binary
  to do work that needs the app's `Migrator`.
- A per-project `console` binary, built from your project's
  `src/bin/console.rs`, owns `db:seed`, your `#[command]`-annotated
  handlers, `queue:work`, `schedule:run`, `workflow:work`, and other
  one-shot tasks that need your app's bootstrap, container bindings,
  and registered observers.

Migration commands live on the developer CLI because they have a
deterministic shape that does not depend on your bootstrap. Everything
that needs your service container or your registered seeders lives on
the per-project console binary. See [Console](console.md) for the full
console surface.

## db:seed

Not a `suprnova` CLI command. Run seeders through the per-project
console binary:

```bash
cargo run --bin console -- db:seed
cargo run --bin console -- db:seed --class=UsersSeeder
```

The seeder registry, ordering rules, and the `--class` matching are
covered in [Seeding](seeding.md). The framework ships `db:seed` as a
built-in console command — your scaffold gets it without any wiring on
your side, but you do invoke it through `console`, not through
`suprnova`.

## Summary

| Command | What it does |
|---|---|
| `suprnova make:migration <name>` | Scaffold a new migration file and register it in `Migrator` |
| `suprnova migrate` | Run pending migrations |
| `suprnova migrate:status` | Show applied/pending status |
| `suprnova migrate:rollback [--step N]` | Roll back the last `N` migrations (default 1) |
| `suprnova migrate:fresh` | Drop all tables and re-run every migration |
| `suprnova db:sync [--skip-migrations] [--regenerate-models]` | Regenerate SeaORM entities from the live schema |
| `cargo run --bin console -- db:seed` | Run registered seeders (per-project console, not the `suprnova` CLI) |

## Next

- [Migrations](migrations.md) — schema-builder API: tables, columns,
  indexes, foreign keys
- [Seeding](seeding.md) — authoring seeders and the `db:seed` console
  command
- [Console](console.md) — the per-project `console` binary and
  `#[command]` handlers
- [Database](database.md) — connections, drivers, transactions, the
  query builder
- [CLI Overview](cli.md) — every `suprnova` subcommand at a glance
