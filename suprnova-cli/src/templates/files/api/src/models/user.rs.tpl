//! User model.
//!
//! In a production app backed by a real database, replace the stub helpers
//! below with actual SeaORM queries. The `id` / `email` fields mirror the
//! `users` table created by the bundled migration.

pub struct User {
    pub id: i64,
    pub email: String,
}

impl User {
    /// Example data -- replace with a real database query.
    pub fn all_example() -> Vec<User> {
        vec![
            User { id: 1, email: "alice@example.com".to_string() },
            User { id: 2, email: "bob@example.com".to_string() },
        ]
    }

    /// Example find-by-id -- replace with a real database query.
    pub fn find_example(id: i64) -> Option<User> {
        Self::all_example().into_iter().find(|u| u.id == id)
    }
}
