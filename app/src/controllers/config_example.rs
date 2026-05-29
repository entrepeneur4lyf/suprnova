use suprnova::{Config, Request, Response, json_response};

use crate::config::{DatabaseConfig, MailConfig};

/// Example endpoint demonstrating how to read application config from a
/// route handler.
///
/// **Principle: expose non-secret metadata only — never connection
/// strings or credentials.** The handler reads the same
/// `DatabaseConfig` and `MailConfig` registrations the rest of the app
/// uses, but only returns operational metadata that is safe to surface
/// in a JSON response: the database *type* (driver family), pool
/// numbers, and the mail *driver name*. The database URL, mail host,
/// port, username, password, and from-address are deliberately
/// withheld — they may contain credentials, internal hostnames, or
/// other secrets that don't belong in a public response.
///
/// Copy-pasting this pattern into a real app? Keep the rule: surface
/// the shape of the config, not the secrets inside it.
pub async fn show(_req: Request) -> Response {
    let db = Config::get::<DatabaseConfig>().unwrap();
    let mail = Config::get::<MailConfig>().unwrap();

    json_response!({
        "message": "Config metadata loaded from .env (safe fields only)",
        "database": {
            "driver": format!("{:?}", db.database_type()),
            "max_connections": db.max_connections,
            "min_connections": db.min_connections,
            "connect_timeout": db.connect_timeout,
            "logging": db.logging
        },
        "mail": {
            "driver": mail.driver,
            "from_name": mail.from_name
        }
    })
}
