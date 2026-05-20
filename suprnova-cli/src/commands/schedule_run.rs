//! schedule:run command - Run all due scheduled tasks once

use crate::ui;
use std::process::Command;

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
