//! #381a — `suprnova db:sync` reports filesystem / subprocess / runtime
//! failures as clean user-facing errors (`ui::error` + exit code 1) instead
//! of aborting with a Rust panic backtrace.
//!
//! Closes the db_sync.rs portion of the Codex final-review "Low: CLI commands
//! still panic instead of returning user-facing errors". The fix matches the
//! convention db_sync.rs already used for its handled errors (the
//! not-in-a-project / DATABASE_URL-missing / connect-failure paths): print a
//! message via `ui::error` and `std::process::exit(1)`.
//!
//! Teeth: against the pre-#381a code the blocked-entities-dir path executed
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
    // fails because its parent is a regular file. Pre-#381a that `.expect`
    // panicked; now it must surface as a clean error.
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
