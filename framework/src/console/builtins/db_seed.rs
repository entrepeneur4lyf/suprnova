//! `db:seed` — runs every registered seeder via
//! [`crate::seed::run_all`].
//!
//! On an empty seeder registry this emits a single
//! `tracing::warn!` and returns `Ok(())` — that's the correct product
//! behavior for "user ran the command before registering anything"
//! and it makes the command safe to invoke from test suites that
//! haven't seeded anything specific.
//!
//! # Targeted runs (`--class=<Name>`)
//!
//! `db:seed --class=UserSeeder` (or the equivalent positional form
//! `db:seed UserSeeder`) runs only the named seeder via
//! [`crate::seed::run_one`]. Unknown names surface as
//! `FrameworkError::not_found("no seeder registered for `X`")`
//! through the normal dispatch path — non-zero exit + diagnostic on
//! stderr. Matches Laravel's `php artisan db:seed --class=UserSeeder`.

use crate::error::FrameworkError;
use crate::seed;
use suprnova_macros::command;

#[command(
    name = "db:seed",
    description = "Run seeders (all by default, or one via --class=<Name>)"
)]
async fn db_seed(args: Vec<String>) -> Result<(), FrameworkError> {
    let class = parse_class_arg(&args)?;

    if seed::count() == 0 {
        // Two channels by design: eprintln so the user actually
        // sees feedback in the absence of a configured tracing
        // subscriber; tracing::warn so observability tools still
        // pick it up in production.
        eprintln!("db:seed: no seeders registered — nothing to run");
        tracing::warn!("db:seed: no seeders registered — nothing to run");
        return Ok(());
    }

    match class {
        Some(name) => seed::run_one(&name).await,
        None => seed::run_all().await,
    }
}

/// Extract the target class name from CLI args. Accepts:
///
/// - `--class=Name`
/// - `--class Name`
/// - `Name` (bare positional, Laravel-compatible)
///
/// Returns `Ok(None)` when no class is specified (run-all). Returns
/// `Err(FrameworkError::bad_request(...))` for malformed flag usage
/// such as `--class` with no following value.
fn parse_class_arg(args: &[String]) -> Result<Option<String>, FrameworkError> {
    let mut iter = args.iter();
    while let Some(arg) = iter.next() {
        if let Some(rest) = arg.strip_prefix("--class=") {
            if rest.is_empty() {
                return Err(FrameworkError::bad_request(
                    "db:seed --class= requires a seeder name (e.g. --class=UserSeeder)",
                ));
            }
            return Ok(Some(rest.to_string()));
        }
        if arg == "--class" {
            let Some(name) = iter.next() else {
                return Err(FrameworkError::bad_request(
                    "db:seed --class requires a seeder name (e.g. --class UserSeeder)",
                ));
            };
            if name.starts_with("--") {
                return Err(FrameworkError::bad_request(
                    "db:seed --class requires a seeder name, found a flag",
                ));
            }
            return Ok(Some(name.clone()));
        }
        if !arg.starts_with("--") {
            // Bare positional name — Laravel-compatible form.
            return Ok(Some(arg.clone()));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn no_args_returns_none() {
        assert_eq!(parse_class_arg(&[]).unwrap(), None);
    }

    #[test]
    fn equals_form_parses_name() {
        assert_eq!(
            parse_class_arg(&s(&["--class=UserSeeder"])).unwrap(),
            Some("UserSeeder".to_string())
        );
    }

    #[test]
    fn space_form_parses_name() {
        assert_eq!(
            parse_class_arg(&s(&["--class", "UserSeeder"])).unwrap(),
            Some("UserSeeder".to_string())
        );
    }

    #[test]
    fn bare_positional_parses_name() {
        assert_eq!(
            parse_class_arg(&s(&["UserSeeder"])).unwrap(),
            Some("UserSeeder".to_string())
        );
    }

    #[test]
    fn empty_equals_rejected() {
        let err = parse_class_arg(&s(&["--class="])).unwrap_err();
        assert!(format!("{err}").contains("requires a seeder name"));
    }

    #[test]
    fn missing_value_after_space_rejected() {
        let err = parse_class_arg(&s(&["--class"])).unwrap_err();
        assert!(format!("{err}").contains("requires a seeder name"));
    }

    #[test]
    fn flag_after_class_rejected() {
        let err = parse_class_arg(&s(&["--class", "--force"])).unwrap_err();
        assert!(format!("{err}").contains("found a flag"));
    }
}
