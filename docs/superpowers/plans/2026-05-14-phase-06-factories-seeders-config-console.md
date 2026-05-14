# Phase 6: Factories + Seeders + Configuration + Console Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Ship the four "Laravel-dev day-one expectations" that don't fit earlier phases: model factories (`UserFactory::new().count(10).create().await`) for tests and seed data, seeders runnable via `suprnova db:seed`, typed configuration with `Config::resolve::<T>()`, and Console command generation (`suprnova make:command Greet` → runnable as `suprnova greet`).

**Architecture:** Factories are derive-macro-driven (`#[derive(Factory)]`) on SeaORM model structs; they fill fields with `fake`-crate generators. Seeders are concrete types implementing a `Seeder` trait; the registry runs them in declared order. Configuration leans on `serde` deserialization from `.env` + a typed struct with `#[config]` attribute marking it env-loadable. Console commands are structs implementing a `Command` trait, collected via `inventory::submit!` so the CLI can list and dispatch them by name without touching the CLI source on every addition.

**Tech Stack:** `fake` 2.x (with `chrono`, `uuid` features), `envy` 0.4 (typed env deserialize, additive to existing `Config::env`), no new console crates (built on `clap` we already pull).

---

## File Structure

**New files:**
- `framework/src/factory/mod.rs` — `Factory` trait, `FactoryBuilder<M>`, generator helpers
- `framework/src/factory/sequence.rs` — `Sequence<T>` for sequential values (1, 2, 3, ...)
- `framework/src/seed/mod.rs` — `Seeder` trait, `SeederRegistry`, `run_seeders` entrypoint
- `framework/src/config/typed.rs` — `Config::resolve::<T>()` reading via envy
- `framework/src/console/mod.rs` — `Command` trait, `CommandRegistry`, dispatch
- `suprnova-macros/src/factory.rs` — `#[derive(Factory)]`
- `suprnova-macros/src/config.rs` — `#[config]` attribute macro
- `suprnova-macros/src/command.rs` — `#[command]` attribute macro
- `suprnova-cli/src/commands/seed.rs` — `suprnova db:seed`
- `suprnova-cli/src/commands/make_command.rs` — `suprnova make:command`
- `suprnova-cli/src/commands/run_command.rs` — dispatch user-defined console commands
- `framework/tests/factory.rs`, `framework/tests/seeders.rs`, `framework/tests/typed_config.rs`, `framework/tests/console.rs`
- `app/src/factories/user_factory.rs` — dogfood
- `app/src/seeders/users_seeder.rs` — dogfood
- `app/src/commands/greet.rs` — dogfood console command

---

## Task 1: Add deps

**Files:** `framework/Cargo.toml`

- [ ] **Step 1: Add**

```toml
# framework/Cargo.toml
fake = { version = "2", features = ["chrono", "uuid"] }
envy = "0.4"
```

- [ ] **Step 2: Verify build**

```bash
cargo check --workspace
```

- [ ] **Step 3: Commit**

```bash
git add framework/Cargo.toml Cargo.lock
git commit -m "feat(deps): add fake, envy for Phase 6"
```

---

## Task 2: Factory trait + manual usage

**Files:** `framework/src/factory/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/factory.rs
use suprnova::factory::Factory;

#[derive(Debug, Clone)]
struct User {
    pub id: i64,
    pub name: String,
    pub email: String,
}

struct UserFactory;

impl Factory for UserFactory {
    type Model = User;
    fn definition() -> User {
        use fake::faker::internet::en::SafeEmail;
        use fake::faker::name::en::Name;
        use fake::Fake;
        User {
            id: 0,
            name: Name().fake(),
            email: SafeEmail().fake(),
        }
    }
}

#[tokio::test]
async fn factory_make_returns_one_instance() {
    let user = UserFactory::new().make();
    assert!(!user.name.is_empty());
    assert!(user.email.contains('@'));
}

#[tokio::test]
async fn factory_count_returns_n_instances() {
    let users = UserFactory::new().count(5).make_many();
    assert_eq!(users.len(), 5);
    // Each has different email — `fake` randomness
    let emails: std::collections::HashSet<_> = users.iter().map(|u| u.email.clone()).collect();
    assert!(emails.len() >= 4, "expected mostly-unique emails");
}

#[tokio::test]
async fn factory_overrides_specific_fields() {
    let user = UserFactory::new()
        .with(|u| u.name = "Alice".into())
        .make();
    assert_eq!(user.name, "Alice");
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/factory/mod.rs
//! Model factories — produce randomized model instances for tests
//! and seed data.
//!
//! ```ignore
//! impl Factory for UserFactory {
//!     type Model = User;
//!     fn definition() -> User {
//!         use fake::Fake;
//!         use fake::faker::internet::en::SafeEmail;
//!         use fake::faker::name::en::Name;
//!         User {
//!             id: 0,
//!             name: Name().fake(),
//!             email: SafeEmail().fake(),
//!         }
//!     }
//! }
//!
//! let users = UserFactory::new().count(10).make_many();
//! ```

mod sequence;
pub use sequence::Sequence;

pub trait Factory {
    type Model;
    fn definition() -> Self::Model;

    fn new() -> FactoryBuilder<Self::Model>
    where
        Self: Sized,
    {
        FactoryBuilder {
            count: 1,
            overrides: Vec::new(),
            _make: Self::definition,
        }
    }
}

pub struct FactoryBuilder<M> {
    count: usize,
    overrides: Vec<Box<dyn Fn(&mut M)>>,
    _make: fn() -> M,
}

impl<M> FactoryBuilder<M> {
    pub fn count(mut self, n: usize) -> Self {
        self.count = n;
        self
    }

    pub fn with(mut self, f: impl Fn(&mut M) + 'static) -> Self {
        self.overrides.push(Box::new(f));
        self
    }

    pub fn make(self) -> M {
        let mut model = (self._make)();
        for o in &self.overrides {
            o(&mut model);
        }
        model
    }

    pub fn make_many(self) -> Vec<M> {
        let count = self.count;
        let make = self._make;
        let overrides = self.overrides;
        (0..count)
            .map(|_| {
                let mut model = make();
                for o in &overrides {
                    o(&mut model);
                }
                model
            })
            .collect()
    }
}
```

```rust
// framework/src/factory/sequence.rs
use std::sync::atomic::{AtomicI64, Ordering};

/// Monotonic counter used to seed unique-value fields. Each call to
/// `next()` returns the previous value + 1, starting at 1.
///
/// ```ignore
/// static IDS: Sequence = Sequence::new();
/// let id = IDS.next();   // 1
/// let id = IDS.next();   // 2
/// ```
pub struct Sequence {
    counter: AtomicI64,
}

impl Sequence {
    pub const fn new() -> Self {
        Self {
            counter: AtomicI64::new(0),
        }
    }
    pub fn next(&self) -> i64 {
        self.counter.fetch_add(1, Ordering::SeqCst) + 1
    }
    pub fn reset(&self) {
        self.counter.store(0, Ordering::SeqCst);
    }
}
```

```rust
// framework/src/lib.rs
pub mod factory;
pub use factory::{Factory, Sequence};
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test factory
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/factory framework/src/lib.rs framework/tests/factory.rs
git commit -m "feat(factory): Factory trait + builder + Sequence for monotonic ids"
```

---

## Task 3: Factory `create()` — persist via SeaORM

**Files:** `framework/src/factory/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/factory.rs — append
use suprnova::factory::Persistable;

#[async_trait::async_trait]
impl Persistable for User {
    async fn persist(self) -> Result<Self, suprnova::FrameworkError> {
        // In real code this calls SeaORM ActiveModel::insert.
        // For the test, we use a stub that pretends to assign an id.
        Ok(User { id: 42, ..self })
    }
}

#[tokio::test]
async fn factory_create_persists_via_persistable() {
    let user = UserFactory::new().create().await.unwrap();
    assert_eq!(user.id, 42);
}

#[tokio::test]
async fn factory_create_many_persists_n() {
    let users = UserFactory::new().count(3).create_many().await.unwrap();
    assert_eq!(users.len(), 3);
    assert!(users.iter().all(|u| u.id == 42));
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/factory/mod.rs — append
use crate::FrameworkError;

#[async_trait::async_trait]
pub trait Persistable: Sized + Send {
    async fn persist(self) -> Result<Self, FrameworkError>;
}

impl<M: Persistable + 'static> FactoryBuilder<M> {
    pub async fn create(self) -> Result<M, FrameworkError> {
        self.make().persist().await
    }

    pub async fn create_many(self) -> Result<Vec<M>, FrameworkError> {
        let many = self.make_many();
        let mut out = Vec::with_capacity(many.len());
        for m in many {
            out.push(m.persist().await?);
        }
        Ok(out)
    }
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test factory create
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/factory/mod.rs framework/tests/factory.rs
git commit -m "feat(factory): Persistable + create/create_many on builder"
```

---

## Task 4: Seeder trait + registry

**Files:** `framework/src/seed/mod.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/seeders.rs
use std::sync::atomic::{AtomicUsize, Ordering};
use suprnova::seed::{run_seeders, Seeder};

static RAN: AtomicUsize = AtomicUsize::new(0);

struct UsersSeeder;

#[async_trait::async_trait]
impl Seeder for UsersSeeder {
    fn name() -> &'static str {
        "UsersSeeder"
    }
    async fn run() -> Result<(), suprnova::FrameworkError> {
        RAN.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[tokio::test]
async fn registered_seeder_runs_via_run_seeders() {
    suprnova::seed::register::<UsersSeeder>();
    RAN.store(0, Ordering::SeqCst);
    run_seeders().await.unwrap();
    assert_eq!(RAN.load(Ordering::SeqCst), 1);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/seed/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use std::sync::Mutex;

type SeederFn = fn() -> futures::future::BoxFuture<'static, Result<(), FrameworkError>>;

static REGISTRY: Mutex<Vec<(String, SeederFn)>> = Mutex::new(Vec::new());

#[async_trait]
pub trait Seeder: Send + Sync {
    fn name() -> &'static str
    where
        Self: Sized;
    async fn run() -> Result<(), FrameworkError>
    where
        Self: Sized;
}

pub fn register<S: Seeder + 'static>() {
    let f: SeederFn = || Box::pin(S::run());
    REGISTRY.lock().unwrap().push((S::name().to_string(), f));
}

pub async fn run_seeders() -> Result<(), FrameworkError> {
    let entries = REGISTRY.lock().unwrap().clone();
    for (name, f) in entries {
        tracing::info!(seeder = %name, "running");
        f().await?;
    }
    Ok(())
}
```

```rust
// framework/src/lib.rs
pub mod seed;
pub use seed::Seeder;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test seeders
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/seed framework/src/lib.rs framework/tests/seeders.rs
git commit -m "feat(seed): Seeder trait + ordered registry + run_seeders"
```

---

## Task 5: `suprnova db:seed` CLI command

**Files:** `suprnova-cli/src/commands/seed.rs`

- [ ] **Step 1: Implement**

```rust
// suprnova-cli/src/commands/seed.rs
use anyhow::Result;

pub async fn run() -> Result<()> {
    // The user's app binary defines the seeder registrations via
    // `suprnova::seed::register::<MySeeder>()` in bootstrap. We
    // shell out to `cargo run -- db:seed` so the app's bootstrap
    // executes, registering seeders, then we call run_seeders.
    //
    // Alternative: link directly to the user's crate as a build
    // dependency. The shell-out path is more flexible.
    let status = tokio::process::Command::new("cargo")
        .args(&["run", "--quiet", "--", "__internal_run_seeders"])
        .status()
        .await?;
    if !status.success() {
        anyhow::bail!("seeders failed");
    }
    Ok(())
}
```

> **Subcommand routing:** The user's binary handles the internal subcommand. In `app/src/cmd/main.rs`, after `bootstrap::register().await` returns, check `std::env::args` for `"__internal_run_seeders"` and if present, call `suprnova::seed::run_seeders().await` and exit. Document this in the generator-emitted boilerplate.

- [ ] **Step 2: Wire into CLI**

```rust
// suprnova-cli/src/main.rs — add to the Command enum
#[derive(clap::Subcommand)]
enum Command {
    // ... existing ...
    #[command(name = "db:seed")]
    DbSeed,
}

// ... dispatch ...
Command::DbSeed => commands::seed::run().await?,
```

- [ ] **Step 3: Smoke test**

```bash
# inside the app/ directory
cargo run -p app -- __internal_run_seeders
```

Expected: registered seeders run.

- [ ] **Step 4: Commit**

```bash
git add suprnova-cli/src/commands/seed.rs suprnova-cli/src/main.rs
git commit -m "feat(cli): suprnova db:seed runs registered seeders"
```

---

## Task 6: Typed Config::resolve

**Files:** `framework/src/config/typed.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/typed_config.rs
use serde::Deserialize;
use suprnova::Config;

#[derive(Deserialize, Debug)]
struct MailConfig {
    pub mail_driver: String,
    pub mail_host: String,
    #[serde(default = "default_port")]
    pub mail_port: u16,
}

fn default_port() -> u16 {
    587
}

#[test]
fn resolve_reads_env_into_typed_struct() {
    unsafe {
        std::env::set_var("MAIL_DRIVER", "smtp");
        std::env::set_var("MAIL_HOST", "smtp.example.com");
        std::env::remove_var("MAIL_PORT");
    }
    let cfg: MailConfig = Config::resolve().unwrap();
    assert_eq!(cfg.mail_driver, "smtp");
    assert_eq!(cfg.mail_host, "smtp.example.com");
    assert_eq!(cfg.mail_port, 587);
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/config/typed.rs
//! Typed config resolution via `envy`. The struct's field names
//! (transformed to UPPER_SNAKE) become env var keys.

use crate::FrameworkError;
use serde::de::DeserializeOwned;

pub fn resolve<T: DeserializeOwned>() -> Result<T, FrameworkError> {
    envy::from_env::<T>()
        .map_err(|e| FrameworkError::internal(format!("config: {}", e)))
}

pub fn resolve_prefixed<T: DeserializeOwned>(prefix: &str) -> Result<T, FrameworkError> {
    envy::prefixed(prefix)
        .from_env::<T>()
        .map_err(|e| FrameworkError::internal(format!("config: {}", e)))
}
```

```rust
// framework/src/config/mod.rs — extend the existing Config struct
impl Config {
    pub fn resolve<T: serde::de::DeserializeOwned>() -> Result<T, crate::FrameworkError> {
        typed::resolve()
    }
    pub fn resolve_prefixed<T: serde::de::DeserializeOwned>(prefix: &str) -> Result<T, crate::FrameworkError> {
        typed::resolve_prefixed(prefix)
    }
}
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test typed_config
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/config/typed.rs framework/src/config/mod.rs framework/tests/typed_config.rs
git commit -m "feat(config): Config::resolve<T>() reads typed configs via envy"
```

---

## Task 7: Console Command trait + registry

**Files:** `framework/src/console/mod.rs`, `suprnova-macros/src/command.rs`

- [ ] **Step 1: Write failing test**

```rust
// framework/tests/console.rs
use suprnova::console::{Command, dispatch};

struct GreetCommand;

#[async_trait::async_trait]
impl Command for GreetCommand {
    fn name() -> &'static str {
        "greet"
    }
    fn description() -> &'static str {
        "Print a greeting"
    }
    async fn run(args: Vec<String>) -> Result<(), suprnova::FrameworkError> {
        println!("hello, {}", args.get(0).cloned().unwrap_or_else(|| "world".into()));
        Ok(())
    }
}

#[tokio::test]
async fn dispatch_runs_named_command() {
    suprnova::console::register::<GreetCommand>();
    dispatch("greet", vec!["Alice".into()]).await.unwrap();
}

#[tokio::test]
async fn unknown_command_returns_error() {
    let result = dispatch("not-real", vec![]).await;
    assert!(result.is_err());
}
```

- [ ] **Step 2: Implement**

```rust
// framework/src/console/mod.rs
use crate::FrameworkError;
use async_trait::async_trait;
use std::sync::Mutex;

type CmdFn = fn(Vec<String>) -> futures::future::BoxFuture<'static, Result<(), FrameworkError>>;

#[derive(Clone)]
struct Entry {
    name: &'static str,
    description: &'static str,
    run: CmdFn,
}

static REGISTRY: Mutex<Vec<Entry>> = Mutex::new(Vec::new());

#[async_trait]
pub trait Command: Send + Sync {
    fn name() -> &'static str
    where
        Self: Sized;
    fn description() -> &'static str
    where
        Self: Sized;
    async fn run(args: Vec<String>) -> Result<(), FrameworkError>
    where
        Self: Sized;
}

pub fn register<C: Command + 'static>() {
    let f: CmdFn = |args| Box::pin(C::run(args));
    REGISTRY.lock().unwrap().push(Entry {
        name: C::name(),
        description: C::description(),
        run: f,
    });
}

pub async fn dispatch(name: &str, args: Vec<String>) -> Result<(), FrameworkError> {
    let entry = {
        let g = REGISTRY.lock().unwrap();
        g.iter().find(|e| e.name == name).cloned()
    };
    match entry {
        Some(e) => (e.run)(args).await,
        None => Err(FrameworkError::internal(format!("unknown command: {}", name))),
    }
}

pub fn list() -> Vec<(String, String)> {
    REGISTRY
        .lock()
        .unwrap()
        .iter()
        .map(|e| (e.name.to_string(), e.description.to_string()))
        .collect()
}
```

```rust
// framework/src/lib.rs
pub mod console;
```

- [ ] **Step 3: Run — expect pass**

```bash
cargo test -p suprnova --test console
```

- [ ] **Step 4: Commit**

```bash
git add framework/src/console framework/src/lib.rs framework/tests/console.rs
git commit -m "feat(console): Command trait + registry + dispatch by name"
```

---

## Task 8: `suprnova make:command` generator

**Files:** `suprnova-cli/src/commands/make_command.rs`

- [ ] **Step 1: Implement**

```rust
// suprnova-cli/src/commands/make_command.rs
use anyhow::Result;
use std::path::Path;

pub fn run(name: &str) -> Result<()> {
    let pascal = to_pascal_case(name);
    let kebab = to_kebab_case(name);
    let path = Path::new("src/commands").join(format!("{}.rs", kebab.replace('-', "_")));
    std::fs::create_dir_all(path.parent().unwrap())?;
    if path.exists() {
        anyhow::bail!("{} already exists", path.display());
    }
    let content = format!(
        r#"use suprnova::{{async_trait, FrameworkError}};
use suprnova::console::Command;

pub struct {pascal}Command;

#[async_trait]
impl Command for {pascal}Command {{
    fn name() -> &'static str {{ "{kebab}" }}
    fn description() -> &'static str {{ "TODO: describe {kebab}" }}
    async fn run(_args: Vec<String>) -> Result<(), FrameworkError> {{
        println!("Running {kebab}");
        Ok(())
    }}
}}
"#
    );
    std::fs::write(&path, content)?;
    println!("✓ created {}", path.display());
    println!();
    println!("Register in bootstrap.rs:");
    println!("  suprnova::console::register::<{pascal}Command>();");
    Ok(())
}

fn to_pascal_case(s: &str) -> String {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|p| !p.is_empty())
        .map(|p| {
            let mut chars = p.chars();
            match chars.next() {
                Some(c) => c.to_ascii_uppercase().to_string() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect()
}

fn to_kebab_case(s: &str) -> String {
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() && i > 0 {
            out.push('-');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}
```

- [ ] **Step 2: Wire into CLI**

```rust
// suprnova-cli/src/main.rs
#[derive(clap::Subcommand)]
enum Command {
    // ...
    #[command(name = "make:command")]
    MakeCommand { name: String },
}

Command::MakeCommand { name } => commands::make_command::run(&name)?,
```

- [ ] **Step 3: Smoke test**

```bash
cd /tmp/my-app
cargo run -p suprnova-cli -- make:command Greet
ls src/commands/greet.rs
```

- [ ] **Step 4: Commit**

```bash
git add suprnova-cli
git commit -m "feat(cli): suprnova make:command generator"
```

---

## Task 9: App dogfood

**Files:** `app/src/factories/`, `app/src/seeders/`, `app/src/commands/`, `app/src/bootstrap.rs`

- [ ] **Step 1: UserFactory**

```rust
// app/src/factories/user_factory.rs
use crate::models::User;
use fake::faker::internet::en::SafeEmail;
use fake::faker::name::en::Name;
use fake::Fake;
use suprnova::Factory;

pub struct UserFactory;

impl Factory for UserFactory {
    type Model = User;
    fn definition() -> User {
        User {
            id: 0,
            name: Name().fake(),
            email: SafeEmail().fake(),
            created_at: chrono::Utc::now(),
        }
    }
}
```

- [ ] **Step 2: UsersSeeder**

```rust
// app/src/seeders/users_seeder.rs
use crate::factories::UserFactory;
use suprnova::{async_trait, FrameworkError, Seeder};

pub struct UsersSeeder;

#[async_trait]
impl Seeder for UsersSeeder {
    fn name() -> &'static str {
        "UsersSeeder"
    }
    async fn run() -> Result<(), FrameworkError> {
        UserFactory::new().count(50).create_many().await?;
        Ok(())
    }
}
```

- [ ] **Step 3: Greet command + register**

```rust
// app/src/commands/greet.rs
use suprnova::{async_trait, FrameworkError};
use suprnova::console::Command;

pub struct GreetCommand;

#[async_trait]
impl Command for GreetCommand {
    fn name() -> &'static str { "greet" }
    fn description() -> &'static str { "Print a greeting" }
    async fn run(args: Vec<String>) -> Result<(), FrameworkError> {
        let who = args.into_iter().next().unwrap_or_else(|| "world".into());
        println!("Hello, {}!", who);
        Ok(())
    }
}
```

```rust
// app/src/bootstrap.rs — inside register()
suprnova::seed::register::<crate::seeders::UsersSeeder>();
suprnova::console::register::<crate::commands::GreetCommand>();
```

- [ ] **Step 4: Smoke test**

```bash
cargo run -p app -- db:seed
cargo run -p app -- greet Alice
```

- [ ] **Step 5: Commit**

```bash
git add app/src
git commit -m "feat(app): UserFactory + UsersSeeder + GreetCommand dogfood"
```

---

## Task 10: Workspace lint + verification + roadmap

- [ ] **Step 1: Clippy + tests**

```bash
cargo clippy --workspace -- -D warnings
cargo test --workspace
```

- [ ] **Step 2: ROADMAP update**

Move from "Missing" to "Production-ready":
- Factories
- Seeders
- Typed Config::resolve
- Console commands + make:command

- [ ] **Step 3: Commit + push**

---

## Self-Review

| Spec item | Covered by |
|-----------|------------|
| Factory trait + builder | Task 2 |
| Persistable / create | Task 3 |
| Sequence helper | Task 2 |
| Seeder trait + registry | Task 4 |
| db:seed CLI | Task 5 |
| Typed Config::resolve | Task 6 |
| Console Command trait | Task 7 |
| make:command generator | Task 8 |
| Dogfood | Task 9 |

---

## Execution Handoff

**Subagent-Driven recommended.**
