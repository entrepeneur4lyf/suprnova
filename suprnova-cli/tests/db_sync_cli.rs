//! `suprnova db:sync` reports filesystem / subprocess / runtime / database
//! failures as clean user-facing errors (`ui::error` + exit code 1) instead
//! of aborting with a Rust panic backtrace.
//!
//! Print a message via `ui::error` and `std::process::exit(1)` is the
//! contract for every failure path in db_sync.rs.
//!
//! Teeth: against the original code the blocked-entities-dir path executed
//! `fs::create_dir_all(...).expect("Failed to create entities directory")`,
//! so the process aborted with `thread 'main' panicked at ...` and a
//! backtrace. The assertions below require a non-zero exit, a human-readable
//! message, AND the absence of `"panicked"` — the last is what proves the
//! panic became a user-facing error.

use std::fs;
use std::process::{Command, Output};

use sea_orm::{ConnectionTrait, Database};
use tempfile::tempdir;

const BIN: &str = env!("CARGO_BIN_EXE_suprnova");

/// Combined stdout + stderr, since the `ui` helpers may write to either stream.
fn combined(out: &Output) -> String {
    let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
    s.push_str(&String::from_utf8_lossy(&out.stderr));
    s
}

#[test]
fn db_sync_outside_a_project_exits_with_clean_error_not_panic() {
    let dir = tempdir().expect("create tempdir");
    let out = Command::new(BIN)
        .arg("db:sync")
        .current_dir(dir.path())
        .output()
        .expect("spawn suprnova binary");

    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "db:sync outside a project must exit 1; output: {text}"
    );
    assert!(
        text.contains("Not in a Suprnova project"),
        "must print a user-facing message; got: {text}"
    );
    assert!(!text.contains("panicked"), "must NOT panic; got: {text}");
}

#[test]
fn db_sync_reports_clean_error_when_entities_dir_cannot_be_created() {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    // A sqlite database with one user table, so schema discovery finds work to
    // do and proceeds to the entity-file generation step (an empty DB would
    // short-circuit at "No tables found").
    let db_path = root.join("schema.db");
    let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
    rt.block_on(async {
        let db = Database::connect(format!("sqlite://{}?mode=rwc", db_path.display()))
            .await
            .expect("connect to sqlite");
        db.execute_unprepared("CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .expect("create widgets table");
    });

    // Stage `src/models` as a FILE: it exists (so we are "in a project" and the
    // `src/models` create is skipped), but creating `src/models/entities/` then
    // fails because its parent is a regular file. The blocked-entities-dir
    // failure must surface as a clean error, not a panic backtrace.
    fs::create_dir_all(root.join("src")).expect("mkdir src");
    fs::write(root.join("src").join("models"), "not a directory").expect("stage src/models file");

    let out = Command::new(BIN)
        .arg("db:sync")
        .env("DATABASE_URL", format!("sqlite://{}", db_path.display()))
        .current_dir(root)
        .output()
        .expect("spawn suprnova binary");

    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "a blocked entities directory must exit 1; output: {text}"
    );
    assert!(
        text.contains("Failed to create entities directory"),
        "must print a user-facing filesystem error; got: {text}"
    );
    assert!(
        !text.contains("panicked"),
        "the filesystem failure must NOT panic; got: {text}"
    );
}

#[test]
fn db_sync_missing_database_url_exits_clean() {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    // Make this a "Suprnova project" (src/models present) so we sail past the
    // project-detection guard and hit `env::var("DATABASE_URL")`.  We isolate
    // DATABASE_URL away — both as a process env var and by writing an empty
    // .env so dotenvy can't backfill it from anywhere.
    fs::create_dir_all(root.join("src/models")).expect("mkdir src/models");
    fs::write(root.join(".env"), "").expect("write empty .env");

    let out = Command::new(BIN)
        .arg("db:sync")
        .arg("--skip-migrations")
        .env_remove("DATABASE_URL")
        .current_dir(root)
        .output()
        .expect("spawn suprnova binary");

    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "missing DATABASE_URL must exit 1; output: {text}"
    );
    assert!(
        text.contains("DATABASE_URL not set"),
        "must print a user-facing DATABASE_URL error; got: {text}"
    );
    assert!(!text.contains("panicked"), "must NOT panic; got: {text}");
}

#[test]
fn db_sync_unreachable_database_exits_clean() {
    let dir = tempdir().expect("create tempdir");
    let root = dir.path();

    // "In a Suprnova project" so we get past the directory guard, no
    // migrations directory so `--skip-migrations` isn't even needed but we
    // pass it anyway for determinism.
    fs::create_dir_all(root.join("src/models")).expect("mkdir src/models");

    // Point DATABASE_URL at a sqlite file in a directory that does NOT exist
    // — `Database::connect` then fails on the open call, and we need that to
    // surface as a clean error.
    let unreachable = root.join("does-not-exist-dir").join("nope.db");

    let out = Command::new(BIN)
        .arg("db:sync")
        .arg("--skip-migrations")
        .env(
            "DATABASE_URL",
            format!("sqlite://{}", unreachable.display()),
        )
        .current_dir(root)
        .output()
        .expect("spawn suprnova binary");

    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "unreachable database must exit 1; output: {text}"
    );
    assert!(
        text.contains("Failed to connect to database"),
        "must print a user-facing connect error; got: {text}"
    );
    assert!(!text.contains("panicked"), "must NOT panic; got: {text}");
}

#[test]
fn db_sync_unreadable_models_mod_exits_clean() {
    // Cover the previously-silent fs::read_to_string fallback in
    // update_models_mod: a permission-blocked `src/models/mod.rs` used to be
    // swallowed by `.unwrap_or_default()`, then overwritten on the next
    // `fs::write` — silently destroying the user's customizations.  Now it
    // must surface as a clean error.
    use std::os::unix::fs::PermissionsExt;

    let dir = tempdir().expect("create tempdir");
    let root = dir.path();
    let db_path = root.join("schema.db");

    let rt = tokio::runtime::Runtime::new().expect("build tokio runtime");
    rt.block_on(async {
        let db = Database::connect(format!("sqlite://{}?mode=rwc", db_path.display()))
            .await
            .expect("connect to sqlite");
        db.execute_unprepared("CREATE TABLE widgets (id INTEGER PRIMARY KEY, name TEXT)")
            .await
            .expect("create widgets table");
    });

    fs::create_dir_all(root.join("src/models/entities")).expect("mkdir entities");
    let mod_path = root.join("src/models/mod.rs");
    fs::write(&mod_path, "//! Application models\n\npub mod custom;\n").expect("seed mod.rs");

    // Strip read permission so fs::read_to_string fails. If we're root the
    // OS ignores this (root bypasses DAC) and the test is a no-op assertion
    // on a clean run — skip cleanly in that case to keep CI portable.
    let mut perms = fs::metadata(&mod_path).expect("metadata").permissions();
    perms.set_mode(0o000);
    fs::set_permissions(&mod_path, perms).expect("set perms");
    let still_readable = fs::read_to_string(&mod_path).is_ok();
    if still_readable {
        // running as root or on a filesystem that doesn't honor the mode bits
        return;
    }

    let out = Command::new(BIN)
        .arg("db:sync")
        .arg("--skip-migrations")
        .env("DATABASE_URL", format!("sqlite://{}", db_path.display()))
        .current_dir(root)
        .output()
        .expect("spawn suprnova binary");

    // Restore perms so tempdir cleanup can drop the file.
    let mut restore = fs::metadata(&mod_path).expect("metadata").permissions();
    restore.set_mode(0o644);
    let _ = fs::set_permissions(&mod_path, restore);

    let text = combined(&out);
    assert_eq!(
        out.status.code(),
        Some(1),
        "unreadable mod.rs must exit 1; output: {text}"
    );
    assert!(
        text.contains("Failed to read existing models/mod.rs"),
        "must surface the read failure; got: {text}"
    );
    assert!(!text.contains("panicked"), "must NOT panic; got: {text}");
}
