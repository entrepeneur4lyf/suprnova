pub use sea_orm_migration::prelude::*;

mod m20251208_160100_create_users_table;
mod m20251208_200000_create_todos_table;
mod m20251208_220000_create_sessions_table;
mod m20251208_230000_create_remember_tokens_table;
mod m20251208_240000_create_posts_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn MigrationTrait>> {
        vec![
            Box::new(m20251208_160100_create_users_table::Migration),
            Box::new(m20251208_200000_create_todos_table::Migration),
            Box::new(m20251208_220000_create_sessions_table::Migration),
            Box::new(m20251208_230000_create_remember_tokens_table::Migration),
            Box::new(m20251208_240000_create_posts_table::Migration),
        ]
    }
}
