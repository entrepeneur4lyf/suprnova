use std::path::Path;
use std::process::Command;

use crate::ui;

pub fn run() {
    if !Path::new("src/migrations").exists() {
        ui::error("No migrations directory found at src/migrations");
        ui::hint("Run 'suprnova make:migration <name>' to create your first migration.");
        std::process::exit(1);
    }

    ui::warning("Dropping all tables and re-running migrations...");
    ui::warning("This will delete all data in your database!");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "migrate:fresh"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        ui::error("Fresh migration failed");
        std::process::exit(1);
    }
}
