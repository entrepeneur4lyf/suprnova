use std::path::Path;
use std::process::Command;

use crate::ui;

pub fn run() {
    if !Path::new("src/migrations").exists() {
        ui::error("No migrations directory found at src/migrations");
        ui::hint("Run 'suprnova make:migration <name>' to create your first migration.");
        std::process::exit(1);
    }

    ui::info("Checking migration status...");

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "migrate:status"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        ui::error("Failed to get migration status");
        std::process::exit(1);
    }
}
