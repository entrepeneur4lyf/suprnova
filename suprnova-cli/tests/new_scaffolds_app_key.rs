//! Smoke test: `suprnova new` must scaffold a non-empty `APP_KEY`
//! into the generated `.env` (codex review finding #1).
//!
//! Both starter flavors are covered — the default Inertia starter
//! (which writes `.env` via `templates::env`) and the `--api` starter
//! (which writes `.env` via `templates::api::env`). A scaffolded app
//! must boot out-of-the-box without the operator having to mint a
//! key, which means the generator has to produce a valid AES-256 key
//! in URL-safe base64.

use std::process::Command;
use tempfile::TempDir;

/// AES-256 / URL-safe base64 / no padding = 43 characters. We assert
/// the exact length so any future regression that emits a placeholder
/// or short key gets caught.
const EXPECTED_KEY_LEN: usize = 43;

fn read_env(project_dir: &std::path::Path) -> String {
    std::fs::read_to_string(project_dir.join(".env")).expect("scaffolder must write a .env file")
}

fn extract_app_key(env_contents: &str) -> &str {
    env_contents
        .lines()
        .find_map(|line| line.strip_prefix("APP_KEY="))
        .expect("scaffolded .env must contain an APP_KEY= line")
        .trim_matches('"')
}

#[test]
fn new_inertia_project_scaffolds_real_app_key() {
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("new")
        .arg("smoke-app")
        .arg("--no-interaction")
        .arg("--no-git")
        .arg("--frontend")
        .arg("svelte")
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "`suprnova new` should succeed");

    let env_contents = read_env(&tmp.path().join("smoke-app"));
    let key = extract_app_key(&env_contents);

    assert_eq!(
        key.len(),
        EXPECTED_KEY_LEN,
        "APP_KEY must be a 43-char base64-url-no-pad value, got: {key:?}"
    );
    assert!(
        !key.contains('+') && !key.contains('/') && !key.contains('='),
        "APP_KEY must be URL-safe (no +, /, or =), got: {key:?}"
    );

    // Round-trip through the framework loader: the scaffolded key must
    // decode to a real 32-byte AES-256 key. This catches any future
    // regression where the placeholder isn't substituted.
    suprnova::EncryptionKey::from_base64(key)
        .expect("scaffolded APP_KEY must load via EncryptionKey::from_base64");
}

#[test]
fn new_api_project_scaffolds_real_app_key() {
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("new")
        .arg("smoke-api")
        .arg("--no-interaction")
        .arg("--no-git")
        .arg("--api")
        .current_dir(tmp.path())
        .status()
        .unwrap();
    assert!(status.success(), "`suprnova new --api` should succeed");

    let env_contents = read_env(&tmp.path().join("smoke-api"));
    let key = extract_app_key(&env_contents);

    assert_eq!(
        key.len(),
        EXPECTED_KEY_LEN,
        "API APP_KEY must be 43-char base64-url-no-pad, got: {key:?}"
    );
    suprnova::EncryptionKey::from_base64(key)
        .expect("scaffolded API APP_KEY must load via EncryptionKey::from_base64");
}

#[test]
fn two_scaffolds_produce_different_keys() {
    let make_project = |name: &str| -> String {
        let tmp = TempDir::new().unwrap();
        let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
            .arg("new")
            .arg(name)
            .arg("--no-interaction")
            .arg("--no-git")
            .arg("--frontend")
            .arg("svelte")
            .current_dir(tmp.path())
            .status()
            .unwrap();
        assert!(status.success());
        let env = read_env(&tmp.path().join(name));
        extract_app_key(&env).to_string()
    };

    let a = make_project("scaffold-a");
    let b = make_project("scaffold-b");
    assert_ne!(a, b, "each scaffold must mint a fresh APP_KEY");
}
