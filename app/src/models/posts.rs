//! Post model stub
//!
//! Plain-struct dogfood model for Phase 3 authorization demo.
//! Not backed by a SeaORM entity or migration — the table doesn't exist.
//! Stubs `find_by_id` and `delete` so the controller can call them
//! in realistic code without a running database.

use suprnova::FrameworkError;

/// A post authored by a user.
#[derive(Debug, Clone)]
pub struct Post {
    pub id: i32,
    pub author_id: i32,
    pub title: String,
    pub is_public: bool,
}

impl Post {
    /// Look up a post by its primary key.
    ///
    /// Dogfood stub: returns `None` for any `id` that isn't `1` so the
    /// controller's 404 branch is exercised in tests; returns a synthetic
    /// post for `id == 1` owned by user `1`.
    pub async fn find_by_id(id: i32) -> Result<Option<Self>, FrameworkError> {
        if id == 1 {
            Ok(Some(Post {
                id: 1,
                author_id: 1,
                title: "Hello, Suprnova!".to_string(),
                is_public: true,
            }))
        } else {
            Ok(None)
        }
    }

    /// Delete the post.
    ///
    /// Dogfood stub: always succeeds.
    pub async fn delete(self) -> Result<(), FrameworkError> {
        Ok(())
    }
}
