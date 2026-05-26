//! SQL identifier + operator validation used by [`DbTableBuilder`].
//!
//! Audit HIGH `database` #2: the model-less query builder
//! ([`DbTableBuilder`]) interpolates table names, column names, and
//! operators directly into the SQL string — SeaORM's `Statement` API
//! parameterises *values* but not identifiers, because SQL itself
//! doesn't allow placeholder-bound identifiers. The builder's module
//! docs warn callers to treat identifier args as trusted, but the
//! `impl Into<String>` signature accepts anything and the audit
//! flagged this as a framework-level injection footgun.
//!
//! This module exposes two pure validators:
//!
//! - [`validate_identifier`] — accepts `[A-Za-z_][A-Za-z0-9_]*`,
//!   optionally followed by `.[A-Za-z_][A-Za-z0-9_]*` for one level of
//!   schema qualification (Postgres `public.users`). Length capped at
//!   128 (well under every backend's limit) so the error path bites
//!   before a malformed identifier reaches SeaORM.
//!
//! - [`validate_sql_operator`] — accepts a fixed allowlist of common
//!   SQL comparison operators (case-insensitive for the alpha ones).
//!
//! Both return the input borrow on success so callers can chain into
//! the SQL string. On failure they return [`FrameworkError::param`]
//! with a message that names the bad input + identifies which kind of
//! check failed.
//!
//! These are called from the terminal methods on [`DbTableBuilder`]
//! (`get` / `update` / `delete` / `insert`) right before the SQL is
//! rendered — the fluent builder methods stay infallible so chaining
//! reads naturally, and validation happens once at the I/O boundary.
//!
//! [`DbTableBuilder`]: crate::database::DbTableBuilder

use crate::FrameworkError;

/// Maximum allowed identifier length. All three first-class backends
/// (Postgres 63, MySQL 64, SQLite ~1MB) accept far more, but capping
/// at 128 keeps the validator's error fast on pathological input
/// without ever truncating a real-world table or column name.
const MAX_IDENT_LEN: usize = 128;

/// Validate a SQL identifier (table or column name) for direct
/// interpolation into a query. Accepts an optionally schema-qualified
/// identifier:
///
/// - Bare: `users`, `audit_log`, `user_id`, `_internal`
/// - Schema-qualified: `public.users`, `analytics.events`
///
/// Each segment must match `[A-Za-z_][A-Za-z0-9_]*`. Length capped at
/// [`MAX_IDENT_LEN`]. Returns the input borrow on success so callers
/// can chain into a `format!()`.
///
/// # Errors
///
/// Returns [`FrameworkError::param`] when the input is empty, exceeds
/// the length cap, contains a segment that doesn't match the
/// identifier shape, or carries more than one `.` separator.
pub fn validate_identifier(ident: &str) -> Result<&str, FrameworkError> {
    if ident.is_empty() {
        return Err(FrameworkError::param("SQL identifier cannot be empty"));
    }
    if ident.len() > MAX_IDENT_LEN {
        return Err(FrameworkError::param(format!(
            "SQL identifier '{ident}' exceeds {MAX_IDENT_LEN} characters"
        )));
    }
    let segments: Vec<&str> = ident.split('.').collect();
    if segments.len() > 2 {
        return Err(FrameworkError::param(format!(
            "SQL identifier '{ident}' has more than one '.' separator; \
             at most one level of schema qualification is supported"
        )));
    }
    for segment in segments {
        validate_segment(ident, segment)?;
    }
    Ok(ident)
}

fn validate_segment(full: &str, segment: &str) -> Result<(), FrameworkError> {
    if segment.is_empty() {
        return Err(FrameworkError::param(format!(
            "SQL identifier '{full}' has an empty segment (consecutive '.'?)"
        )));
    }
    let mut chars = segment.chars();
    let first = chars.next().expect("non-empty checked above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return Err(FrameworkError::param(format!(
            "SQL identifier '{full}' segment '{segment}' must start \
             with a letter or '_' (got '{first}')"
        )));
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return Err(FrameworkError::param(format!(
                "SQL identifier '{full}' segment '{segment}' contains \
                 invalid character '{c}' (allowed: A-Z a-z 0-9 _)"
            )));
        }
    }
    Ok(())
}

/// Allowlist of SQL comparison / membership operators accepted by
/// [`DbTableBuilder::filter_op`]. Validated case-insensitively for
/// the alpha operators; punctuation operators are case-irrelevant
/// but matched literally.
///
/// [`DbTableBuilder::filter_op`]: crate::database::DbTableBuilder::filter_op
const ALLOWED_OPERATORS: &[&str] = &[
    "=",
    "<>",
    "!=",
    "<",
    "<=",
    ">",
    ">=",
    "LIKE",
    "NOT LIKE",
    "ILIKE",
    "NOT ILIKE",
    "IS",
    "IS NOT",
];

/// Validate a SQL comparison operator against the allowlist.
///
/// Returns the *canonical* form (matching the allowlist entry — alpha
/// operators are upper-cased) so callers can drop it straight into a
/// `format!()` without worrying about case-folding.
///
/// # Errors
///
/// Returns [`FrameworkError::param`] when the operator is not in the
/// allowlist.
pub fn validate_sql_operator(op: &str) -> Result<&'static str, FrameworkError> {
    let trimmed = op.trim();
    for &canonical in ALLOWED_OPERATORS {
        if canonical.eq_ignore_ascii_case(trimmed) {
            return Ok(canonical);
        }
    }
    Err(FrameworkError::param(format!(
        "SQL operator '{op}' is not in the allowlist; supported: {}",
        ALLOWED_OPERATORS.join(", ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_simple_identifier() {
        assert_eq!(validate_identifier("users").unwrap(), "users");
        assert_eq!(validate_identifier("audit_log").unwrap(), "audit_log");
        assert_eq!(validate_identifier("_internal").unwrap(), "_internal");
        assert_eq!(validate_identifier("col1").unwrap(), "col1");
    }

    #[test]
    fn accepts_schema_qualified() {
        assert_eq!(validate_identifier("public.users").unwrap(), "public.users");
        assert_eq!(
            validate_identifier("analytics.events").unwrap(),
            "analytics.events"
        );
    }

    #[test]
    fn rejects_empty() {
        let err = validate_identifier("").unwrap_err();
        assert!(format!("{err}").contains("cannot be empty"));
    }

    #[test]
    fn rejects_starting_with_digit() {
        let err = validate_identifier("1col").unwrap_err();
        assert!(format!("{err}").contains("must start"));
    }

    #[test]
    fn rejects_injection_payloads() {
        // The audit's whole concern: an attacker-controlled table name
        // like "users; DROP TABLE users; --" must error before it
        // reaches the SQL builder.
        for payload in [
            "users; DROP TABLE users",
            "users--",
            "users WHERE 1=1",
            "users\"; --",
            "users'",
            "users)",
            "users OR 1=1",
            "users\n",
            "users\0",
            "users union select",
        ] {
            assert!(
                validate_identifier(payload).is_err(),
                "must reject injection payload: {payload:?}"
            );
        }
    }

    #[test]
    fn rejects_double_dot() {
        // More than one level of schema qualification.
        assert!(validate_identifier("a.b.c").is_err());
        // Empty middle segment.
        assert!(validate_identifier("a..b").is_err());
        // Leading or trailing dot.
        assert!(validate_identifier(".users").is_err());
        assert!(validate_identifier("users.").is_err());
    }

    #[test]
    fn rejects_too_long() {
        let long = "a".repeat(MAX_IDENT_LEN + 1);
        assert!(validate_identifier(&long).is_err());
    }

    #[test]
    fn operator_allowlist_accepts_canonical_forms() {
        for op in ["=", "<>", "!=", "<", "<=", ">", ">="] {
            assert_eq!(validate_sql_operator(op).unwrap(), op);
        }
    }

    #[test]
    fn operator_allowlist_is_case_insensitive_for_alpha() {
        assert_eq!(validate_sql_operator("like").unwrap(), "LIKE");
        assert_eq!(validate_sql_operator("LIKE").unwrap(), "LIKE");
        assert_eq!(validate_sql_operator("Like").unwrap(), "LIKE");
        assert_eq!(validate_sql_operator("not like").unwrap(), "NOT LIKE");
        assert_eq!(validate_sql_operator("is not").unwrap(), "IS NOT");
    }

    #[test]
    fn operator_allowlist_rejects_unknown() {
        for op in ["||", ";--", "; DROP", "UNION", "OR 1=1", "BETWEEN"] {
            assert!(
                validate_sql_operator(op).is_err(),
                "must reject operator: {op:?}"
            );
        }
    }
}
