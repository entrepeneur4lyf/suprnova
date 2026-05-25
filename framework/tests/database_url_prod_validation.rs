//! Regression: HIGH audit finding `database` #1 — missing `DATABASE_URL`
//! silently boots a local SQLite database through the library facade.
//!
//! `DatabaseConfig::from_env` defaulted to `sqlite://./database.db` when
//! `DATABASE_URL` was unset. In production the operator's intended DB URL
//! is a secret that ABSOLUTELY MUST be present; falling through to a
//! local file silently is a real safety / data-loss vector.
//!
//! The fix tracks where the URL came from (`UrlSource::Env` / `Default`
//! / `Explicit`) and exposes `validate_for_environment` which is called
//! from `DB::init` / `DB::init_with`. Production-like environments
//! (`Production`, `Staging`) refuse the silent fallback;
//! Local/Development/Testing/Custom keep the dev convenience.
//!
//! Tests cover the 4 corners of the source × env matrix that matter:
//!   1. Production + Default source → error (the audit-flagged hole).
//!   2. Production + Env source → ok (operator set DATABASE_URL).
//!   3. Production + Explicit source → ok (builder set URL programmatically).
//!   4. Local + Default source → ok (dev convenience preserved).

use std::sync::Mutex;

use suprnova::config::Environment;
use suprnova::database::config::{DatabaseConfig, UrlSource};

/// Serialize the whole module: every test reads / mutates `DATABASE_URL`
/// indirectly via `DatabaseConfig::from_env`.
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvSnapshot {
    keys: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
    fn capture(keys: &[&'static str]) -> Self {
        Self {
            keys: keys
                .iter()
                .map(|k| (*k, std::env::var(k).ok()))
                .collect(),
        }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        for (k, v) in &self.keys {
            // SAFETY: ENV_LOCK serializes these tests within the suite.
            // Process-wide getenv races against other test binaries are
            // out of scope; the workspace runs each integration test
            // suite in its own binary.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

fn set_env(key: &str, value: Option<&str>) {
    // SAFETY: ENV_LOCK held by the caller.
    unsafe {
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }
}

#[test]
fn production_with_default_source_refuses_silent_sqlite_fallback() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    // The whole point: DATABASE_URL is unset.
    set_env("DATABASE_URL", None);

    let cfg = DatabaseConfig::from_env();
    assert_eq!(
        cfg.url_source,
        UrlSource::Default,
        "unset DATABASE_URL must produce Default source"
    );
    assert_eq!(cfg.url, DatabaseConfig::DEFAULT_SQLITE_URL);

    let result = cfg.validate_for_environment(&Environment::Production);
    let err = result.expect_err(
        "production + Default source MUST fail-closed; this is the audit fix",
    );
    let msg = format!("{err}");
    assert!(
        msg.contains("DATABASE_URL is required"),
        "error must call out the missing env var by name; got: {msg}"
    );
    assert!(
        msg.contains(DatabaseConfig::DEFAULT_SQLITE_URL),
        "error must name the fallback URL so operators know what was almost used; got: {msg}"
    );
}

#[test]
fn production_with_env_source_is_accepted() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    set_env("DATABASE_URL", Some("postgres://prod.example/app"));

    let cfg = DatabaseConfig::from_env();
    assert_eq!(cfg.url_source, UrlSource::Env);
    assert_eq!(cfg.url, "postgres://prod.example/app");

    cfg.validate_for_environment(&Environment::Production)
        .expect("operator-supplied DATABASE_URL must pass production validation");
}

#[test]
fn production_with_explicit_source_is_accepted_even_for_sqlite() {
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    set_env("DATABASE_URL", None);

    // Builder path → Explicit, even when the URL happens to be SQLite.
    // The point of UrlSource is that "operator chose this URL" is
    // different from "fallback kicked in" — both can end up at SQLite,
    // and the validator must accept the explicit case.
    let cfg = DatabaseConfig::builder()
        .url("sqlite://./prod-on-purpose.db")
        .build();
    assert_eq!(cfg.url_source, UrlSource::Explicit);

    cfg.validate_for_environment(&Environment::Production)
        .expect("builder-supplied URL must pass production validation regardless of scheme");
}

#[test]
fn staging_with_default_source_also_refuses() {
    // Staging is production-like for the purposes of "real secrets must
    // be configured." This guards against the regression where the
    // is-prod check is too narrow.
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    set_env("DATABASE_URL", None);

    let cfg = DatabaseConfig::from_env();
    let err = cfg
        .validate_for_environment(&Environment::Staging)
        .expect_err("staging + Default source MUST fail-closed");
    assert!(format!("{err}").contains("DATABASE_URL is required"));
}

#[test]
fn local_with_default_source_is_accepted() {
    // The whole point of keeping a Default source is that
    // `cargo run` in a fresh checkout boots against
    // `sqlite://./database.db` with zero setup. This documents the
    // intended dev posture as a hard-coded test guarantee.
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    set_env("DATABASE_URL", None);

    let cfg = DatabaseConfig::from_env();
    assert_eq!(cfg.url_source, UrlSource::Default);

    cfg.validate_for_environment(&Environment::Local)
        .expect("Local + Default source must keep working — dev zero-setup posture");
    cfg.validate_for_environment(&Environment::Development)
        .expect("Development + Default source must keep working");
    cfg.validate_for_environment(&Environment::Testing)
        .expect("Testing + Default source must keep working — tests use sqlite::memory: or fresh files");
}

#[test]
fn is_configured_reflects_url_source() {
    // The legacy `is_configured` API is preserved but its semantics
    // now derive from `UrlSource` instead of string-comparing the URL.
    // A user who explicitly sets `DATABASE_URL=sqlite://./database.db`
    // is "configured" — they meant the local SQLite, didn't fall
    // through to it.
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["DATABASE_URL"]);

    set_env("DATABASE_URL", None);
    assert!(
        !DatabaseConfig::from_env().is_configured(),
        "Default source must not count as configured"
    );

    set_env(
        "DATABASE_URL",
        Some("sqlite://./database.db"), // happens to match the fallback string!
    );
    assert!(
        DatabaseConfig::from_env().is_configured(),
        "user who explicitly sets the fallback URL counts as configured; \
         the source-tracking model distinguishes intent from coincidental matches"
    );
}
