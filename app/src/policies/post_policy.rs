//! Policy for Post resources.
//!
//! Demonstrates Phase 3 `#[policy]` macro: each method becomes a named
//! gate (`view-post`, `update-post`, `delete-post`) registered via
//! `inventory::submit!` at link time and collected by
//! `suprnova::authorization::init_policies()` at boot.

use crate::models::{posts::Post, users::User};
use suprnova::policy;

/// Authorization rules for the Post resource.
pub struct PostPolicy;

#[policy(User, Post)]
impl PostPolicy {
    /// Anyone can view a public post; authentication is still required
    /// to reach this gate via `BearerTokenMiddleware`.
    fn view(_user: &User, post: &Post) -> bool {
        post.is_public
    }

    /// Only the author may edit their own post.
    fn update(user: &User, post: &Post) -> bool {
        post.author_id == user.id
    }

    /// The author or an admin may delete a post.
    ///
    /// `is_admin()` is a helper method on User because the dogfood entity
    /// has no `is_admin` column — a real app would persist this flag and
    /// add it to the migration.
    fn delete(user: &User, post: &Post) -> bool {
        post.author_id == user.id || user.is_admin()
    }
}
