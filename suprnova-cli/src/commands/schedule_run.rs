//! schedule:run command - Run all due scheduled tasks once

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
    ui::info("Running due scheduled tasks...");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "schedule:run"])
        .status();

    interpret_cargo_status(status, "schedule:run", false)
}
