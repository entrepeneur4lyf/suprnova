//! Tests for `.env` loading precedence (codex review finding #12).
//!
//! The loader must:
//! 1. Load base `.env` BEFORE detecting `APP_ENV`, so an `APP_ENV` value
//!    that lives only in `.env` correctly selects environment-specific
//!    files for the same invocation.
//! 2. Apply file precedence: `.env` (lowest) <
//!    `.env.local` < `.env.<env>` < `.env.<env>.local` < system env.
//! 3. Preserve real system env values — files never override what was
//!    set in the actual process environment when `load_dotenv` ran.
//!
//! These tests mutate `std::env` (process-global) so we serialize them
//! under a single `Mutex` and capture/restore the keys we touch.

use std::path::Path;
use std::sync::Mutex;

use suprnova::config::{load_dotenv, Environment};

/// Serialize the whole module: every test mutates `APP_ENV` etc.
static ENV_LOCK: Mutex<()> = Mutex::new(());

/// RAII snapshot for the env keys this suite mutates. On drop, restores
/// the original values so a failing assertion doesn't poison the next
/// test in the binary.
struct EnvSnapshot {
    keys: Vec<(&'static str, Option<String>)>,
}

impl EnvSnapshot {
    fn capture(keys: &[&'static str]) -> Self {
        let captured = keys
            .iter()
            .map(|k| (*k, std::env::var(k).ok()))
            .collect();
        Self { keys: captured }
    }
}

impl Drop for EnvSnapshot {
    fn drop(&mut self) {
        for (k, v) in &self.keys {
            // SAFETY: ENV_LOCK serializes these tests within the suite.
            // Concurrent getenv races on other tests are bounded by the
            // process-wide mutex used by every env-touching test.
            unsafe {
                match v {
                    Some(val) => std::env::set_var(k, val),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

fn write_env_file(dir: &Path, name: &str, contents: &str) {
    std::fs::write(dir.join(name), contents).expect("write env file");
}

#[test]
fn app_env_in_base_dotenv_selects_environment_specific_file() {
    // Codex finding #12: when only the base `.env` contains
    // `APP_ENV=production`, the loader must still load `.env.production`
    // on the same call. Before the fix, `Environment::detect` ran
    // BEFORE the base `.env` was loaded, so `.env.production` was
    // skipped.

    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&[
        "APP_ENV",
        "DOTENV_TEST_VAR_BASE_ONLY",
        "DOTENV_TEST_VAR_PROD",
        "DOTENV_TEST_VAR_LOCAL",
    ]);
    // SAFETY: serialized via ENV_LOCK
    unsafe {
        std::env::remove_var("APP_ENV");
        std::env::remove_var("DOTENV_TEST_VAR_BASE_ONLY");
        std::env::remove_var("DOTENV_TEST_VAR_PROD");
        std::env::remove_var("DOTENV_TEST_VAR_LOCAL");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    write_env_file(
        dir,
        ".env",
        "APP_ENV=production\nDOTENV_TEST_VAR_BASE_ONLY=base\n",
    );
    write_env_file(dir, ".env.production", "DOTENV_TEST_VAR_PROD=prod\n");
    write_env_file(dir, ".env.local", "DOTENV_TEST_VAR_LOCAL=local\n");

    let env = load_dotenv(dir);

    assert_eq!(env, Environment::Production);
    assert_eq!(
        std::env::var("APP_ENV").as_deref(),
        Ok("production"),
        "base .env must seed APP_ENV"
    );
    assert_eq!(
        std::env::var("DOTENV_TEST_VAR_PROD").as_deref(),
        Ok("prod"),
        ".env.production must load when APP_ENV is set in .env (the codex finding fix)"
    );
    assert_eq!(
        std::env::var("DOTENV_TEST_VAR_BASE_ONLY").as_deref(),
        Ok("base"),
        "base .env values must survive"
    );
    assert_eq!(
        std::env::var("DOTENV_TEST_VAR_LOCAL").as_deref(),
        Ok("local"),
        ".env.local must also load"
    );
}

#[test]
fn system_env_app_env_selects_environment_file() {
    // The "old" path — `APP_ENV=production` in real system env — must
    // still work after the reorder.
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["APP_ENV", "DOTENV_TEST_SYS_PROD"]);
    // SAFETY: serialized via ENV_LOCK
    unsafe {
        std::env::remove_var("DOTENV_TEST_SYS_PROD");
        std::env::set_var("APP_ENV", "production");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    write_env_file(dir, ".env.production", "DOTENV_TEST_SYS_PROD=ok\n");

    let env = load_dotenv(dir);

    assert_eq!(env, Environment::Production);
    assert_eq!(std::env::var("DOTENV_TEST_SYS_PROD").as_deref(), Ok("ok"));
}

#[test]
fn no_app_env_defaults_to_local() {
    // Backwards compatibility: when APP_ENV is unset and no .env exists
    // anywhere, the loader returns Local. We preserve this so existing
    // local-dev workflows that rely on `cargo run` keep working.
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["APP_ENV"]);
    // SAFETY: serialized via ENV_LOCK
    unsafe {
        std::env::remove_var("APP_ENV");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let env = load_dotenv(tmp.path());

    assert_eq!(env, Environment::Local);
}

#[test]
fn system_env_wins_over_dotenv_files() {
    // Real system env must beat every file. Without the system-env
    // restore at the end of `load_dotenv`, an env-specific file using
    // `from_path_override` would clobber a system value — that would
    // be a precedence inversion (system env is highest, files are
    // lower).
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["APP_ENV", "DOTENV_TEST_SYS_WINS"]);
    // SAFETY: serialized via ENV_LOCK
    unsafe {
        std::env::set_var("APP_ENV", "production");
        std::env::set_var("DOTENV_TEST_SYS_WINS", "from-system");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    // Files try to set the same key — system env value must survive.
    write_env_file(dir, ".env", "DOTENV_TEST_SYS_WINS=from-base\n");
    write_env_file(
        dir,
        ".env.production",
        "DOTENV_TEST_SYS_WINS=from-env-specific\n",
    );

    load_dotenv(dir);

    assert_eq!(
        std::env::var("DOTENV_TEST_SYS_WINS").as_deref(),
        Ok("from-system"),
        "system env value must win over any file"
    );
}

#[test]
fn env_specific_file_overrides_base_dotenv() {
    // `.env.production` must beat `.env` for the same key, because
    // env-specific files are more specific (higher precedence than the
    // base file).
    let _guard = ENV_LOCK.lock().unwrap();
    let _snap = EnvSnapshot::capture(&["APP_ENV", "DOTENV_TEST_OVERRIDE"]);
    // SAFETY: serialized via ENV_LOCK
    unsafe {
        std::env::remove_var("APP_ENV");
        std::env::remove_var("DOTENV_TEST_OVERRIDE");
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path();
    write_env_file(
        dir,
        ".env",
        "APP_ENV=production\nDOTENV_TEST_OVERRIDE=from-base\n",
    );
    write_env_file(
        dir,
        ".env.production",
        "DOTENV_TEST_OVERRIDE=from-prod\n",
    );

    load_dotenv(dir);

    assert_eq!(
        std::env::var("DOTENV_TEST_OVERRIDE").as_deref(),
        Ok("from-prod"),
        "env-specific file must override base .env for shared keys"
    );
}
