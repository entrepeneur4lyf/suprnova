//! workflow:work command - Run the workflow worker daemon

use crate::commands::interpret_cargo_status;
use crate::ui;
use std::process::Command;

pub fn run() {
    if let Err(e) = run_inner() {
        ui::error(&e);
        std::process::exit(1);
    }
}

fn run_inner() -> Result<(), String> {
    ui::info("Starting workflow worker...");
    ui::hint("Press Ctrl+C to stop");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "workflow:work"])
        .status();

    interpret_cargo_status(status, "workflow:work", true)?;

    ui::br();
    ui::info("Workflow worker stopped.");
    Ok(())
}
