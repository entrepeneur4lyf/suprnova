//! `greet` — dogfood for the framework's `#[command]` macro.
//!
//! Demonstrates the simplest possible user-defined console command:
//! single attribute, async fn, parses args, prints to stdout.
//!
//! ```text
//! cargo run --bin console -- greet              # "Hello, world!"
//! cargo run --bin console -- greet Alice        # "Hello, Alice!"
//! cargo run --bin console -- greet Alice Bob    # "Hello, Alice and Bob!"
//! ```
//!
//! The function is also callable directly from Rust (`greet(vec![...])`)
//! — the macro preserves the original fn — which is what the integration
//! test in `app/tests/console_greet.rs` exercises.

use suprnova::{command, FrameworkError};

#[command(name = "greet", description = "Print a friendly greeting")]
pub async fn greet(args: Vec<String>) -> Result<(), FrameworkError> {
    let message = match args.len() {
        0 => "Hello, world!".to_string(),
        1 => format!("Hello, {}!", args[0]),
        _ => {
            let (last, rest) = args.split_last().expect("non-empty by match arm");
            format!("Hello, {} and {}!", rest.join(", "), last)
        }
    };
    println!("{message}");
    Ok(())
}
