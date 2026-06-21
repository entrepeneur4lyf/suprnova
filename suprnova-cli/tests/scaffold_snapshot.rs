//! Golden tests for `suprnova new`.
//!
//! Codex review finding #6 (S4 follow-up): the generated `--api` and
//! default Inertia starters previously shipped TODO-laden bodies, stub
//! `all_example` / `find_example` helpers, and an in-memory Torii config
//! that was unsafe for production. These tests make the absence of those
//! markers a hard guarantee:
//!
//! 1. Walk every generated `.rs`, `.toml`, and template-driven config
//!    file and reject occurrences of `TODO`, `FIXME`, `unimplemented!`,
//!    `panic!(`, or the historical stub helpers.
//! 2. Confirm that a freshly scaffolded project compiles end-to-end by
//!    rewriting its `suprnova` dependency to point at the in-tree
//!    framework crate and running `cargo check`. Marked `#[ignore]` so
//!    `cargo test --workspace` stays fast — run with
//!    `cargo test --workspace -- --ignored` to exercise them.

use std::path::{Path, PathBuf};
use std::process::Command;

use tempfile::TempDir;

/// Substring markers that must NEVER appear in scaffolder output. We
/// search literal substrings rather than comment-only patterns because
/// the production templates have no legitimate reason to include any
/// of these — every prior occurrence was a punted implementation.
const FORBIDDEN_MARKERS: &[&str] = &[
    "TODO",
    "FIXME",
    "unimplemented!",
    "panic!(",
    "all_example",
    "find_example",
    "sqlite_in_memory",
];

/// File extensions whose contents we audit. The scaffolder writes
/// frontend files (`.tsx`, `.svelte`, `.vue`, `.ts`, `.json`,
/// `.html`, `.css`) plus Rust source and TOML manifests — every one is
/// fair game for stub markers. We deliberately do *not* audit binary
/// blobs or lockfiles.
const AUDITED_EXTENSIONS: &[&str] = &[
    "rs", "toml", "ts", "tsx", "svelte", "vue", "json", "html", "css", "yml", "yaml",
];

/// Paths whose substrings indicate generated / external content we
/// don't own. `target/` and `node_modules/` won't exist before
/// `cargo check` / `npm install`, but the filter is cheap and matches
/// what users would see after a real boot.
const SKIP_PATH_FRAGMENTS: &[&str] = &["/target/", "/node_modules/"];

fn cli_binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_suprnova"))
}

/// Run `suprnova new ...` inside `tmp` and return the project root.
fn scaffold(tmp: &TempDir, project_name: &str, extra_args: &[&str]) {
    let mut cmd = Command::new(cli_binary());
    cmd.arg("new")
        .arg(project_name)
        .arg("--no-interaction")
        .arg("--no-git");
    for a in extra_args {
        cmd.arg(a);
    }
    let output = cmd
        .current_dir(tmp.path())
        .output()
        .expect("`suprnova new` should run");
    assert!(
        output.status.success(),
        "`suprnova new {project_name} {extra_args:?}` failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

/// Walk the scaffolded project and collect any forbidden-marker hits.
fn collect_marker_hits(root: &Path) -> Vec<(PathBuf, &'static str, usize, String)> {
    let mut hits = Vec::new();
    for entry in walkdir::WalkDir::new(root)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let path = entry.path();
        let path_str = path.to_string_lossy();
        if SKIP_PATH_FRAGMENTS
            .iter()
            .any(|frag| path_str.contains(frag))
        {
            continue;
        }
        let Some(ext) = path.extension().and_then(|e| e.to_str()) else {
            continue;
        };
        if !AUDITED_EXTENSIONS.contains(&ext) {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(path) else {
            continue;
        };
        for (line_idx, line) in content.lines().enumerate() {
            for marker in FORBIDDEN_MARKERS {
                if line.contains(marker) {
                    hits.push((
                        path.to_owned(),
                        *marker,
                        line_idx + 1,
                        line.trim().to_string(),
                    ));
                }
            }
        }
    }
    hits
}

fn assert_no_marker_hits(root: &Path) {
    let hits = collect_marker_hits(root);
    assert!(
        hits.is_empty(),
        "scaffolded project under {} contains forbidden stub markers:\n{}",
        root.display(),
        hits.iter()
            .map(|(p, marker, line, text)| {
                format!("  {}:{} [{}] {}", p.display(), line, marker, text)
            })
            .collect::<Vec<_>>()
            .join("\n"),
    );
}

// ---------------------------------------------------------------------------
// Stub-marker audits (fast — no compilation involved).
// ---------------------------------------------------------------------------

#[test]
fn api_starter_has_no_stub_markers() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "marker_api", &["--api"]);
    assert_no_marker_hits(&tmp.path().join("marker_api"));
}

#[test]
fn default_starter_has_no_stub_markers_svelte() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "marker_svelte", &["--frontend", "svelte"]);
    assert_no_marker_hits(&tmp.path().join("marker_svelte"));
}

#[test]
fn default_starter_has_no_stub_markers_react() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "marker_react", &["--frontend", "react"]);
    assert_no_marker_hits(&tmp.path().join("marker_react"));
}

#[test]
fn default_starter_has_no_stub_markers_vue() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "marker_vue", &["--frontend", "vue"]);
    assert_no_marker_hits(&tmp.path().join("marker_vue"));
}

// ---------------------------------------------------------------------------
// Auth contract audits — the scaffolded model must keep the trait overrides
// the login flow depends on.
// ---------------------------------------------------------------------------

/// The scaffolded `User` must override `Authenticatable::get_auth_password`
/// to expose the stored hash. The trait default returns `None`, which makes
/// `EloquentUserProvider::validate_credentials` reject every password — a
/// freshly scaffolded app's `Auth::attempt` login 422s on correct
/// credentials. The project still compiles without the override, so only a
/// content check catches the regression.
#[test]
fn scaffolded_user_model_exposes_auth_password() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "auth_pw", &["--frontend", "svelte"]);
    let model = tmp.path().join("auth_pw").join("src/models/user.rs");
    let body = std::fs::read_to_string(&model).expect("scaffolded user model should exist");
    assert!(
        body.contains("fn get_auth_password"),
        "scaffolded User must override get_auth_password or Auth::attempt \
         rejects every password:\n{body}"
    );
    assert!(
        body.contains("Some(&self.password)"),
        "get_auth_password must return the stored hash:\n{body}"
    );
}

// ---------------------------------------------------------------------------
// Inertia client pin — the scaffolds must ship a current `@inertiajs/*`
// adapter. A typo or accidental downgrade compiles fine (npm isn't part of
// `cargo check`), so only a content assertion catches drift.
// ---------------------------------------------------------------------------

/// The expected `@inertiajs/*` caret pin every frontend scaffold ships.
/// Bump this in lockstep with the `package.json.tpl` files.
const EXPECTED_INERTIA_PIN: &str = "^3.4.0";

fn assert_inertia_pin(tmp: &TempDir, project: &str, frontend: &str, adapter: &str) {
    scaffold(tmp, project, &["--frontend", frontend]);
    let pkg = tmp
        .path()
        .join(project)
        .join("frontend")
        .join("package.json");
    let body = std::fs::read_to_string(&pkg).expect("scaffolded package.json should exist");
    let needle = format!("\"@inertiajs/{adapter}\": \"{EXPECTED_INERTIA_PIN}\"");
    assert!(
        body.contains(&needle),
        "{frontend} scaffold must pin @inertiajs/{adapter} at {EXPECTED_INERTIA_PIN}:\n{body}"
    );
}

#[test]
fn scaffold_pins_current_inertia_client() {
    let tmp = TempDir::new().unwrap();
    assert_inertia_pin(&tmp, "inertia_svelte", "svelte", "svelte");
    assert_inertia_pin(&tmp, "inertia_react", "react", "react");
    assert_inertia_pin(&tmp, "inertia_vue", "vue", "vue3");
}

// ---------------------------------------------------------------------------
// make:* generator audits — ensure the in-place commands emit clean code.
// ---------------------------------------------------------------------------

#[test]
fn make_middleware_generates_clean_code() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "make_mid", &["--frontend", "svelte"]);
    let project = tmp.path().join("make_mid");

    let status = Command::new(cli_binary())
        .args(["make:middleware", "RateLimit"])
        .current_dir(&project)
        .status()
        .expect("`suprnova make:middleware` should run");
    assert!(status.success(), "make:middleware should succeed");

    assert_no_marker_hits(&project.join("src/middleware/rate_limit.rs"));
}

#[test]
fn make_action_generates_clean_code() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "make_act", &["--frontend", "svelte"]);
    let project = tmp.path().join("make_act");

    let status = Command::new(cli_binary())
        .args(["make:action", "SendInvoice"])
        .current_dir(&project)
        .status()
        .expect("`suprnova make:action` should run");
    assert!(status.success(), "make:action should succeed");

    assert_no_marker_hits(&project.join("src/actions/send_invoice_action.rs"));
}

#[test]
fn make_task_generates_clean_code() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "make_tsk", &["--frontend", "svelte"]);
    let project = tmp.path().join("make_tsk");

    // make:task creates the tasks directory itself if missing.
    let status = Command::new(cli_binary())
        .args(["make:task", "RotateAuditLog"])
        .current_dir(&project)
        .status()
        .expect("`suprnova make:task` should run");
    assert!(status.success(), "make:task should succeed");

    // Walk the project — the generated task file is what we want clean,
    // but a broader sweep also confirms nothing else regressed.
    assert_no_marker_hits(&project);
}

// ---------------------------------------------------------------------------
// --with-portless contract: portless.json present with the flag, absent without.
// ---------------------------------------------------------------------------

#[test]
fn with_portless_emits_portless_json() {
    let tmp = TempDir::new().unwrap();
    scaffold(
        &tmp,
        "portless_app",
        &["--frontend", "svelte", "--with-portless"],
    );
    let pj = tmp.path().join("portless_app").join("portless.json");
    assert!(
        pj.is_file(),
        "portless.json should be present when --with-portless is passed"
    );
    let body = std::fs::read_to_string(&pj).unwrap();
    assert!(
        body.contains("\"name\": \"portless_app\""),
        "portless.json name field wrong:\n{body}"
    );
    assert!(
        body.contains("\"appPort\": 8765"),
        "portless.json appPort field wrong:\n{body}"
    );
}

#[test]
fn without_portless_omits_portless_json() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "plain_app", &["--frontend", "svelte"]);
    assert!(
        !tmp.path().join("plain_app").join("portless.json").exists(),
        "portless.json must be absent without --with-portless"
    );
}

// ---------------------------------------------------------------------------
// Compile checks — slow (full transitive build); ignored by default.
// ---------------------------------------------------------------------------

/// Rewrite the scaffolded `Cargo.toml` so it builds against the
/// in-tree `framework/` crate instead of the published `suprnova`
/// release. The published crate exists for end users but isn't
/// resolvable inside this workspace's test harness; swapping the dep
/// line for a `path =` reference is what lets `cargo check` succeed
/// before the crate is on crates.io.
fn patch_local_suprnova(project: &Path) {
    let framework_dir = workspace_framework_dir();
    let cargo_toml = project.join("Cargo.toml");
    let original = std::fs::read_to_string(&cargo_toml).expect("read scaffolded Cargo.toml");

    let mut rewritten = String::with_capacity(original.len());
    let mut replaced = false;
    for line in original.lines() {
        if line.trim_start().starts_with("suprnova = ") {
            rewritten.push_str(&format!(
                "suprnova = {{ path = \"{}\" }}\n",
                framework_dir.display(),
            ));
            replaced = true;
        } else {
            rewritten.push_str(line);
            rewritten.push('\n');
        }
    }
    assert!(
        replaced,
        "scaffolded Cargo.toml must contain a `suprnova = ...` dependency line"
    );
    std::fs::write(&cargo_toml, rewritten).expect("write patched Cargo.toml");
}

fn workspace_framework_dir() -> PathBuf {
    // `CARGO_MANIFEST_DIR` points at `suprnova-cli/`, so the sibling
    // workspace member lives one level up.
    let here = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    here.parent()
        .expect("suprnova-cli must have a workspace parent")
        .join("framework")
}

fn run_cargo_check(project: &Path) {
    let output = Command::new(env!("CARGO"))
        .arg("check")
        .current_dir(project)
        .output()
        .expect("cargo check should run");
    assert!(
        output.status.success(),
        "scaffolded project at {} failed cargo check.\n\
         stdout:\n{}\nstderr:\n{}",
        project.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
}

#[test]
#[ignore = "compile check — runs `cargo check` on a scaffolded project; slow"]
fn api_starter_compiles_with_cargo_check() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "compile_api", &["--api"]);
    let project = tmp.path().join("compile_api");
    patch_local_suprnova(&project);
    run_cargo_check(&project);
}

#[test]
#[ignore = "compile check — runs `cargo check` on a scaffolded project; slow"]
fn default_starter_compiles_with_cargo_check() {
    let tmp = TempDir::new().unwrap();
    scaffold(&tmp, "compile_default", &["--frontend", "svelte"]);
    let project = tmp.path().join("compile_default");
    patch_local_suprnova(&project);
    run_cargo_check(&project);
}
