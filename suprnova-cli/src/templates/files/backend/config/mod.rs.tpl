mod database;
mod mail;

pub use database::DatabaseConfig;
pub use mail::MailConfig;

use suprnova::{Config, DatabaseConfig as SuprnovaDatabaseConfig};

/// Register all application configs
pub fn register_all() {
    // Use Suprnova's built-in DatabaseConfig
    Config::register(SuprnovaDatabaseConfig::from_env());
    Config::register(MailConfig::from_env());
}
