//! schedule:run command - Run all due scheduled tasks once

use std::process::Command;
use crate::ui;

pub fn run() {
    ui::info("Running due scheduled tasks...");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "schedule:run"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        ui::error("Schedule run failed");
        std::process::exit(1);
    }
}
