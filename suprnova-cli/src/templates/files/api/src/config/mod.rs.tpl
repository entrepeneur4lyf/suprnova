use suprnova::{Config, DatabaseConfig};

/// Register all application configuration.
pub fn register_all() {
    Config::register(DatabaseConfig::from_env());
}
