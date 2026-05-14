//! workflow:work command - Run the workflow worker daemon

use std::process::Command;
use crate::ui;

pub fn run() {
    ui::info("Starting workflow worker...");
    ui::hint("Press Ctrl+C to stop");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "workflow:work"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success() {
        if let Some(code) = status.code() {
            if code != 130 {
                ui::error(&format!("Workflow worker exited with error (code: {})", code));
                std::process::exit(1);
            }
        }
    }

    ui::br();
    ui::info("Workflow worker stopped.");
}
