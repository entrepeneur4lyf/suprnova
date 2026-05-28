[package]
name = "{package_name}"
version = "0.1.0"
edition = "2021"

[[bin]]
name = "{package_name}"
path = "src/main.rs"

# Per-project console binary — runtime command dispatch (db:seed,
# user-defined `#[command]` async fns, etc.).
[[bin]]
name = "console"
path = "src/bin/console.rs"

[dependencies]
suprnova = "0.1"
tokio = { version = "1", features = ["full"] }
sea-orm-migration = { version = "1.0", features = ["sqlx-sqlite", "sqlx-postgres", "runtime-tokio-native-tls"] }
sea-orm = { version = "1.0", features = ["sqlx-sqlite", "sqlx-postgres", "runtime-tokio-native-tls", "macros", "with-chrono"] }
serde = { version = "1.0", features = ["derive"] }
serde_json = "1.0"
async-trait = "0.1"
dotenvy = "0.15"
validator = { version = "0.20", features = ["derive"] }
chrono = { version = "0.4", features = ["serde"] }
