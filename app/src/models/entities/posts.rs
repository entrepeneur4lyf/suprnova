// AUTO-GENERATED-LIKE FILE — mirror of the shape `suprnova db:sync`
// would emit for the posts table. Hand-written here because Phase 3
// codex review finding #17 retired the stub `models/posts.rs` and the
// dogfood app doesn't currently run `db:sync` between migrations.
// Add custom code to src/models/posts.rs instead.

use sea_orm::entity::prelude::*;
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel, Serialize)]
#[sea_orm(table_name = "posts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i32,
    pub author_id: i32,
    pub title: String,
    pub body: String,
    pub is_public: bool,
    pub created_at: String,
    pub updated_at: String,
}

// Note: Relation enum is required here for DeriveEntityModel macro.
// Define your actual relations in src/models/posts.rs using the Related trait.
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
