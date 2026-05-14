use std::path::Path;
use std::process::Command;

use crate::ui;

pub fn run(step: u32) {
    if !Path::new("src/migrations").exists() {
        ui::error("No migrations directory found at src/migrations");
        ui::hint("Make sure you're in a Suprnova project root directory.");
        std::process::exit(1);
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
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        ui::error("Rollback failed");
        std::process::exit(1);
    }
}
