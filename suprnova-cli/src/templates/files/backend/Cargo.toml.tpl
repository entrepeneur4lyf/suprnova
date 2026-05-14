[package]
name = "{package_name}"
version = "0.1.0"
edition = "2021"
description = "{description}"
{authors_line}
[[bin]]
name = "{package_name}"
path = "cmd/main.rs"

[dependencies]
suprnova = {{ package = "suprnova-rs", version = "0.1" }}
tokio = {{ version = "1", features = ["full"] }}
sea-orm-migration = {{ version = "1.0", features = ["sqlx-sqlite", "sqlx-postgres", "runtime-tokio-native-tls"] }}
sea-orm = {{ version = "1.0", features = ["sqlx-sqlite", "sqlx-postgres", "runtime-tokio-native-tls", "macros"] }}
serde = {{ version = "1.0", features = ["derive"] }}
async-trait = "0.1"
dotenvy = "0.15"
clap = {{ version = "4", features = ["derive"] }}
chrono = {{ version = "0.4", features = ["serde"] }}
validator = {{ version = "0.18", features = ["derive"] }}
