//! schedule:list command - Display all registered scheduled tasks

use crate::ui;
use std::process::Command;

pub fn run() {
    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "schedule:list"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        ui::error("Failed to list scheduled tasks");
        std::process::exit(1);
    }
}
