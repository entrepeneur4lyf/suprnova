use suprnova::{json_response, Config, Request, Response};

use crate::config::{DatabaseConfig, MailConfig};

/// Example endpoint showing how to use config values
pub async fn show(_req: Request) -> Response {
    let db = Config::get::<DatabaseConfig>().unwrap();
    let mail = Config::get::<MailConfig>().unwrap();

    json_response!({
        "message": "Config values loaded from .env",
        "database": {
            "url": db.url,
            "max_connections": db.max_connections,
            "min_connections": db.min_connections,
            "connect_timeout": db.connect_timeout,
            "logging": db.logging
        },
        "mail": {
            "driver": mail.driver,
            "host": mail.host,
            "port": mail.port,
            "from_address": mail.from_address,
            "from_name": mail.from_name
        }
    })
}
