//! web:run command - Run the web server

use std::process::Command;
use crate::ui;

pub fn run() {
    ui::info("Starting web server...");
    ui::hint("Press Ctrl+C to stop");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "web:run"])
        .status()
        .expect("Failed to execute cargo command");

    if !status.success()
        && let Some(code) = status.code()
            && code != 130 {
                ui::error("Web server exited with error");
                std::process::exit(1);
            }

    ui::br();
    ui::info("Web server stopped.");
}
