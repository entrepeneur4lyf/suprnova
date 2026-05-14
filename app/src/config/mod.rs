mod database;
mod mail;

pub use database::DatabaseConfig;
pub use mail::MailConfig;

use suprnova::{Config, DatabaseConfig as SupernovaDatabaseConfig};

/// Register all application configs
pub fn register_all() {
    // Use Suprnova's built-in DatabaseConfig
    Config::register(SupernovaDatabaseConfig::from_env());
    Config::register(MailConfig::from_env());
}
