use std::process::Command;
use tempfile::TempDir;

#[test]
fn make_inertia_with_data_flag_emits_data_derive() {
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("make:inertia")
        .arg("UserProps")
        .arg("--data")
        .current_dir(&tmp)
        .status()
        .unwrap();
    assert!(status.success(), "make:inertia --data failed");

    let generated = std::fs::read_to_string(tmp.path().join("src/props/user_props.rs"))
        .expect("expected generated file at src/props/user_props.rs");
    assert!(
        generated.contains("#[derive(Data, Validate)]")
            || generated.contains("#[derive(suprnova::Data, validator::Validate)]"),
        "expected Data + Validate derive, got: {}",
        generated
    );
    assert!(
        !generated.contains("InertiaProps"),
        "Data struct should not contain InertiaProps, got: {}",
        generated
    );
}

#[test]
fn make_inertia_without_flag_does_not_create_props_file() {
    // Without --data, make:inertia creates a frontend page (requires frontend/src/pages/).
    // In a bare TempDir with no project structure, the command exits non-zero because
    // frontend/src/pages/ does not exist — this is the expected current behaviour.
    // We verify: (a) the command fails gracefully, (b) no app/src/props/ file is created.
    let tmp = TempDir::new().unwrap();
    let status = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("make:inertia")
        .arg("UserProps")
        .current_dir(&tmp)
        .status()
        .unwrap();
    // Should exit non-zero — no frontend/src/pages/ directory.
    assert!(
        !status.success(),
        "make:inertia without --data should fail outside a project root"
    );

    // No props file should have been created.
    let props_path = tmp.path().join("src/props/user_props.rs");
    assert!(
        !props_path.exists(),
        "make:inertia without --data must not create src/props/user_props.rs"
    );
}

#[test]
fn make_inertia_data_warns_on_first_time_props_dir_creation() {
    // First invocation creates src/props/ — should emit a warning
    // pointing at src/lib.rs so the user remembers to add the module
    // declaration. Without the warning the generated file is orphaned
    // and the new Data struct is invisible to the rest of the crate.
    let tmp = TempDir::new().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("make:inertia")
        .arg("UserProps")
        .arg("--data")
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(output.status.success(), "make:inertia --data failed");

    let combined = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        combined.contains("pub mod props"),
        "first-time warning must name the missing declaration; got:\n{combined}"
    );
    assert!(
        combined.contains("lib.rs"),
        "first-time warning must name the file to edit; got:\n{combined}"
    );
}

#[test]
fn make_inertia_data_does_not_warn_when_props_dir_already_exists() {
    // Second invocation (dir exists) must NOT spam the user with the
    // first-time warning — the assumption is that whoever created the
    // directory already declared the module.
    let tmp = TempDir::new().unwrap();
    std::fs::create_dir_all(tmp.path().join("src/props")).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_suprnova"))
        .arg("make:inertia")
        .arg("UserProps")
        .arg("--data")
        .current_dir(&tmp)
        .output()
        .unwrap();
    assert!(output.status.success(), "make:inertia --data failed");

    let combined = String::from_utf8_lossy(&output.stdout).to_string()
        + &String::from_utf8_lossy(&output.stderr);
    assert!(
        !combined.contains("pub mod props"),
        "warning must NOT fire when src/props already exists; got:\n{combined}"
    );
}
