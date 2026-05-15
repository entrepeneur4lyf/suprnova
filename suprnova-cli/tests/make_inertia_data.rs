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

    let generated = std::fs::read_to_string(tmp.path().join("app/src/props/user_props.rs"))
        .expect("expected generated file at app/src/props/user_props.rs");
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
    let props_path = tmp.path().join("app/src/props/user_props.rs");
    assert!(
        !props_path.exists(),
        "make:inertia without --data must not create app/src/props/user_props.rs"
    );
}
