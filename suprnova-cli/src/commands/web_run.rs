//! web:run command - Run the web server

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
    ui::info("Starting web server...");
    ui::hint("Press Ctrl+C to stop");
    ui::br();

    let status = Command::new("cargo")
        .args(["run", "--quiet", "--", "web:run"])
        .status();

    interpret_cargo_status(status, "web:run", true)?;

    ui::br();
    ui::info("Web server stopped.");
    Ok(())
}
