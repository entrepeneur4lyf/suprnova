//! {project_name} — JSON:API Server Entry Point

use suprnova::Application;

use {package_name}::{bootstrap, config, migrations, routes};

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
