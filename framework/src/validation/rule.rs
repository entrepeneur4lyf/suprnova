//! Rule objects — composable validators that work alongside (and
//! independently of) `#[derive(Validate)]`.
//!
//! Two traits cover the synchronous and asynchronous design space:
//!
//! - [`Rule`] — pure sync check on a single value. Built-ins:
//!   [`rules::Required`], [`rules::Email`], [`rules::Min`],
//!   [`rules::Max`].
//! - [`AsyncRule`] — async check (DB queries — [`async_rules::Unique`]
//!   lives here).

/// A synchronous validator over a single string value.
///
/// `Err(msg)` carries a human-readable message describing why the
/// value failed. Suprnova does not impose a translation scheme on the
/// message — wrap [`Rule`] yourself if you need i18n.
pub trait Rule {
    /// Check `value`. Return `Ok(())` if it passes, `Err(message)` if
    /// it fails.
    fn passes(&self, value: &str) -> Result<(), String>;
}

/// Built-in synchronous rules.
pub mod rules {
    use super::Rule;
    use validator::ValidateEmail;

    /// Laravel `required` — value must be present and non-whitespace.
    pub struct Required;
    impl Rule for Required {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.trim().is_empty() {
                Err("required".into())
            } else {
                Ok(())
            }
        }
    }

    /// Laravel `email` — defers to [`validator::ValidateEmail`] so
    /// semantics match `#[validate(email)]` on derived types.
    pub struct Email;
    impl Rule for Email {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.validate_email() {
                Ok(())
            } else {
                Err("must be a valid email".into())
            }
        }
    }

    /// Laravel `min:N` — value must be at least `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes, so multi-byte
    /// characters count as a single character.
    pub struct Min(pub usize);
    impl Rule for Min {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() >= self.0 {
                Ok(())
            } else {
                Err(format!("must be at least {} characters", self.0))
            }
        }
    }

    /// Laravel `max:N` — value must be at most `N` characters long.
    ///
    /// Counts Unicode scalar values (`char`s), not bytes.
    pub struct Max(pub usize);
    impl Rule for Max {
        fn passes(&self, value: &str) -> Result<(), String> {
            if value.chars().count() <= self.0 {
                Ok(())
            } else {
                Err(format!("must be at most {} characters", self.0))
            }
        }
    }
}

/// An asynchronous validator over a single string value.
///
/// Rules that need to hit a database, an HTTP service, or any other
/// `.await` point go here. [`async_rules::Unique`] is the canonical
/// built-in.
#[async_trait::async_trait]
pub trait AsyncRule: Send + Sync {
    /// Check `value`. Return `Ok(())` if it passes, `Err(message)` if
    /// it fails.
    async fn passes(&self, value: &str) -> Result<(), String>;
}

/// Built-in asynchronous rules.
pub mod async_rules {
    use super::AsyncRule;
    use crate::DB;
    use sea_orm::{ConnectionTrait, Statement, Value};

    /// Laravel `unique:table,column[,except_id]` — issues a single
    /// parameterized `COUNT(*)` against the configured DB connection.
    ///
    /// Returns `Err` when at least one row matches and its `id`
    /// (when [`Self::except_id`] is set) differs.
    ///
    /// # Safety on identifiers
    ///
    /// `table` and `column` are `&'static str` slices under crate
    /// control (i.e. caller-provided literals in source). The
    /// implementation interpolates them directly into the SQL string,
    /// which is safe because they are not user-controlled. The actual
    /// value being checked and the `except_id`, on the other hand, are
    /// passed as bound parameters.
    pub struct Unique {
        pub table: &'static str,
        pub column: &'static str,
        pub except_id: Option<i64>,
    }

    #[async_trait::async_trait]
    impl AsyncRule for Unique {
        async fn passes(&self, value: &str) -> Result<(), String> {
            let conn = DB::connection().map_err(|e| format!("db: {e}"))?;
            let backend = conn.inner().get_database_backend();

            let (sql, values) = match self.except_id {
                None => (
                    format!(
                        "SELECT COUNT(*) AS c FROM {} WHERE {} = ?",
                        self.table, self.column
                    ),
                    vec![Value::from(value.to_string())],
                ),
                Some(id) => (
                    format!(
                        "SELECT COUNT(*) AS c FROM {} WHERE {} = ? AND id <> ?",
                        self.table, self.column
                    ),
                    vec![Value::from(value.to_string()), Value::from(id)],
                ),
            };

            let stmt = Statement::from_sql_and_values(backend, &sql, values);
            let row = conn
                .inner()
                .query_one(stmt)
                .await
                .map_err(|e| format!("unique query: {e}"))?
                .ok_or_else(|| "unique query returned no rows".to_string())?;

            let count: i64 = row
                .try_get::<i64>("", "c")
                .map_err(|e| format!("unique decode: {e}"))?;

            if count == 0 {
                Ok(())
            } else {
                Err(format!(
                    "{} already exists for {}",
                    self.column, self.table
                ))
            }
        }
    }
}

pub use async_rules::Unique;
