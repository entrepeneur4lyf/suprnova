//! `Config::resolve` / `Config::resolve_prefixed` integration tests.
//!
//! Every test is `#[serial]` because env-var manipulation is process-
//! global. Tests set the env vars they care about at the top, run the
//! resolve, then unset to keep subsequent tests isolated.

use serde::Deserialize;
use serial_test::serial;
use suprnova::Config;

/// SAFETY: every caller is inside a `#[serial]` test, so no concurrent
/// thread mutates env vars. `std::env::set_var` / `remove_var` are
/// `unsafe` since Rust 1.74 on `getenv`-using platforms; the marker
/// lives here.
fn set_env(k: &str, v: &str) {
    unsafe { std::env::set_var(k, v) }
}
fn clear_env(keys: &[&str]) {
    for k in keys {
        unsafe { std::env::remove_var(k) }
    }
}

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
#[serial]
fn resolve_reads_env_into_typed_struct_with_defaults() {
    clear_env(&["MAIL_DRIVER", "MAIL_HOST", "MAIL_PORT"]);
    set_env("MAIL_DRIVER", "smtp");
    set_env("MAIL_HOST", "smtp.example.com");
    // MAIL_PORT intentionally unset → serde default fires.

    let cfg: MailConfig = Config::resolve().expect("resolve from env");
    assert_eq!(cfg.mail_driver, "smtp");
    assert_eq!(cfg.mail_host, "smtp.example.com");
    assert_eq!(
        cfg.mail_port, 587,
        "missing field falls through to serde default"
    );

    clear_env(&["MAIL_DRIVER", "MAIL_HOST", "MAIL_PORT"]);
}

#[test]
#[serial]
fn resolve_parses_typed_fields_from_string_envs() {
    clear_env(&["MAIL_DRIVER", "MAIL_HOST", "MAIL_PORT"]);
    set_env("MAIL_DRIVER", "ses");
    set_env("MAIL_HOST", "email.us-east-1.amazonaws.com");
    set_env("MAIL_PORT", "2525");

    let cfg: MailConfig = Config::resolve().unwrap();
    assert_eq!(cfg.mail_port, 2525, "u16 parsed from \"2525\"");

    clear_env(&["MAIL_DRIVER", "MAIL_HOST", "MAIL_PORT"]);
}

#[test]
#[serial]
fn resolve_reports_missing_required_field() {
    // MAIL_DRIVER is required (no serde default) — missing should error.
    clear_env(&["MAIL_DRIVER", "MAIL_HOST", "MAIL_PORT"]);
    // Leave MAIL_DRIVER unset; set the others.
    set_env("MAIL_HOST", "x");

    let err = Config::resolve::<MailConfig>().unwrap_err();
    let msg = format!("{err}");
    assert!(
        msg.contains("config: failed to resolve") && msg.contains("MailConfig"),
        "error mentions Config + the failing type: {msg}"
    );
    assert!(
        msg.to_lowercase().contains("mail_driver") || msg.to_lowercase().contains("missing"),
        "error mentions the missing field or 'missing': {msg}"
    );

    clear_env(&["MAIL_HOST"]);
}

#[derive(Deserialize, Debug)]
struct PrefixedDbConfig {
    pub host: String,
    pub port: u16,
    #[serde(default)]
    pub ssl: bool,
}

#[test]
#[serial]
fn resolve_prefixed_strips_prefix_before_mapping() {
    clear_env(&["DB_HOST", "DB_PORT", "DB_SSL"]);
    set_env("DB_HOST", "db.example.com");
    set_env("DB_PORT", "5432");
    // DB_SSL omitted → default false.

    let cfg: PrefixedDbConfig = Config::resolve_prefixed("DB_").expect("resolve with DB_ prefix");
    assert_eq!(cfg.host, "db.example.com");
    assert_eq!(cfg.port, 5432);
    assert!(!cfg.ssl, "missing field uses serde default");

    clear_env(&["DB_HOST", "DB_PORT", "DB_SSL"]);
}

#[test]
#[serial]
fn resolve_prefixed_ignores_unprefixed_env_vars() {
    clear_env(&["DB_HOST", "DB_PORT", "DB_SSL", "HOST", "PORT"]);
    // Set BOTH prefixed and unprefixed; resolve_prefixed must only see
    // the prefixed ones, so HOST=ghost / PORT=9999 must NOT leak into
    // PrefixedDbConfig.
    set_env("HOST", "ghost.example.com");
    set_env("PORT", "9999");
    set_env("DB_HOST", "real-db.example.com");
    set_env("DB_PORT", "5432");

    let cfg: PrefixedDbConfig = Config::resolve_prefixed("DB_").unwrap();
    assert_eq!(
        cfg.host, "real-db.example.com",
        "the unprefixed HOST=ghost.example.com was correctly ignored"
    );
    assert_eq!(cfg.port, 5432);

    clear_env(&["DB_HOST", "DB_PORT", "DB_SSL", "HOST", "PORT"]);
}

#[derive(Deserialize, Debug)]
struct RenamedConfig {
    #[serde(rename = "weird_external_name")]
    pub internal_field: String,
}

#[test]
#[serial]
fn resolve_respects_serde_rename() {
    clear_env(&["WEIRD_EXTERNAL_NAME", "INTERNAL_FIELD"]);
    // Set the renamed key, NOT the internal field name. If serde
    // rename were ignored, we'd hit a missing-field error.
    set_env("WEIRD_EXTERNAL_NAME", "via-rename");

    let cfg: RenamedConfig = Config::resolve().unwrap();
    assert_eq!(cfg.internal_field, "via-rename");

    clear_env(&["WEIRD_EXTERNAL_NAME"]);
}
