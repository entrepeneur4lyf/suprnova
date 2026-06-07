use std::path::Path;
use std::process::Command;

use crate::commands::interpret_cargo_status;
use crate::ui;

pub fn run(step: u32) {
    if let Err(e) = run_inner(step) {
        ui::error(&e);
        std::process::exit(1);
    }
}

fn run_inner(step: u32) -> Result<(), String> {
    if !Path::new("src/migrations").exists() {
        ui::hint("Make sure you're in a Suprnova project root directory.");
        return Err("No migrations directory found at src/migrations".to_string());
    }

    ui::info(&format!("Rolling back {} migration(s)...", step));

    let status = Command::new("cargo")
        .args([
            "run",
            "--quiet",
            "--",
            "migrate:rollback",
            &step.to_string(),
        ])
        .status();

    interpret_cargo_status(status, "migrate:rollback", false)
}
