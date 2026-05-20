//! schedule:work command - Run the scheduler daemon

use crate::ui;
use std::process::Command;

pub fn run() {
    ui::info("Starting scheduler daemon...");
    ui::hint("Press Ctrl+C to stop");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "schedule:work"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success()
        && let Some(code) = status.code()
        && code != 130
    {
        ui::error(&format!(
            "Scheduler daemon exited with error (code: {})",
            code
        ));
        std::process::exit(1);
    }

    ui::br();
    ui::info("Scheduler daemon stopped.");
}
