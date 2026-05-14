//! suprnova Application Entry Point

use app::{bootstrap, config, migrations, routes};
use suprnova::Application;

#[tokio::main]
async fn main() {
    Application::new()
        .config(config::register_all)
        .bootstrap(bootstrap::register)
        .routes(routes::register)
        .migrations::<migrations::Migrator>()
        .run()
        .await;
}
