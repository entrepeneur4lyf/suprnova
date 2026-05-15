use sea_orm_migration::MigratorTrait;

mod m20240101_000001_create_users_table;

pub struct Migrator;

#[async_trait::async_trait]
impl MigratorTrait for Migrator {
    fn migrations() -> Vec<Box<dyn sea_orm_migration::MigrationTrait>> {
        vec![Box::new(m20240101_000001_create_users_table::Migration)]
    }
}
