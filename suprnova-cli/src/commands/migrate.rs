use std::path::Path;
use std::process::Command;

use crate::commands::interpret_cargo_status;
use crate::ui;

pub fn run() {
    if let Err(e) = run_inner() {
        ui::error(&e);
        std::process::exit(1);
    }
}

fn run_inner() -> Result<(), String> {
    if !Path::new("src/migrations").exists() {
        ui::hint("Run 'suprnova make:migration <name>' to create your first migration.");
        return Err("No migrations directory found at src/migrations".to_string());
    }

    ui::info("Running migrations...");

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "migrate"])
        .status();

    interpret_cargo_status(status, "migrate", false)
}
