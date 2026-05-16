//! Integration tests for the rule-object primitives in
//! `suprnova::validation::rule`.

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement, Value};
use suprnova::rules::{Email, Max, Min, Required, RequiredIf, RequiredUnless, RequiredWith};
use suprnova::testing::TestContainer;
use suprnova::{AsyncRule, ContextualRule, DbConnection, FormContext, Rule, Unique};

#[test]
fn required_passes_on_present() {
    let r = Required;
    assert!(r.passes("not empty").is_ok());
    assert!(r.passes("").is_err());
    assert!(
        r.passes("   ").is_err(),
        "all-whitespace counts as empty"
    );
}

#[test]
fn email_accepts_well_formed_addresses() {
    let r = Email;
    assert!(r.passes("user@example.com").is_ok());
    assert!(r.passes("user+filter@sub.example.co.uk").is_ok());
}

#[test]
fn email_rejects_malformed_addresses() {
    let r = Email;
    // The `validator` crate rejects these:
    assert!(r.passes("not-an-email").is_err());
    assert!(r.passes("@nodomain").is_err());
    assert!(r.passes("noatsign.com").is_err());
    assert!(r.passes("trailing.dot@x.").is_err());
}

#[test]
fn min_max_check_length() {
    let r = Min(8);
    assert!(r.passes("longenough").is_ok());
    assert!(r.passes("short").is_err());

    let r = Max(5);
    assert!(r.passes("hi").is_ok());
    assert!(r.passes("toolong").is_err());
}

// --- contextual rules ---

fn ctx(pairs: &[(&str, &str)]) -> FormContext {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect()
}

#[test]
fn required_if_triggers_when_other_field_matches() {
    let rule = RequiredIf {
        other: "billing_type",
        value: "card",
    };
    let c = ctx(&[("billing_type", "card")]);
    assert!(rule.passes("4111111111111111", &c).is_ok());
    assert!(rule.passes("", &c).is_err());

    let c2 = ctx(&[("billing_type", "invoice")]);
    assert!(rule.passes("", &c2).is_ok());
}

#[test]
fn required_with_triggers_when_other_field_present() {
    let rule = RequiredWith {
        other: "address_line_1",
    };
    let c = ctx(&[("address_line_1", "1 Main St")]);
    assert!(rule.passes("12345", &c).is_ok());
    assert!(rule.passes("", &c).is_err());

    let c2 = ctx(&[]);
    assert!(rule.passes("", &c2).is_ok());
}

#[test]
fn required_unless_triggers_when_other_field_does_not_match() {
    let rule = RequiredUnless {
        other: "subscription",
        value: "free",
    };
    let c_free = ctx(&[("subscription", "free")]);
    assert!(rule.passes("", &c_free).is_ok());

    let c_paid = ctx(&[("subscription", "pro")]);
    assert!(rule.passes("billing_token", &c_paid).is_ok());
    assert!(rule.passes("", &c_paid).is_err());
}

// --- async rules (Unique) ---
//
// `TestContainer` is thread-local. The test harness runs tests on a
// thread pool, so each `#[tokio::test]` builds a fresh in-memory
// SQLite, wires it into the current thread's container with
// `TestContainer::singleton`, and runs the assertion. The guard
// returned by `TestContainer::fake()` clears the container on drop.

async fn fresh_db() -> DbConnection {
    let raw = Database::connect("sqlite::memory:").await.unwrap();
    raw.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE users (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL)"
            .to_string(),
    ))
    .await
    .unwrap();
    DbConnection::from_raw(raw)
}

async fn seed_user_with_email(db: &DbConnection, email: &str) -> i64 {
    let backend = db.inner().get_database_backend();
    let stmt = Statement::from_sql_and_values(
        backend,
        "INSERT INTO users (email) VALUES (?)",
        vec![Value::from(email.to_string())],
    );
    let result = db.inner().execute(stmt).await.unwrap();
    result.last_insert_id() as i64
}

#[tokio::test]
async fn unique_passes_when_no_row_exists() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: None,
    };
    assert!(rule.passes("nobody@example.com").await.is_ok());
}

#[tokio::test]
async fn unique_fails_when_row_exists() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    seed_user_with_email(&db, "taken@example.com").await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: None,
    };
    let err = rule.passes("taken@example.com").await.unwrap_err();
    assert!(
        err.contains("already"),
        "expected duplicate-error message, got: {err}"
    );
}

#[tokio::test]
async fn unique_ignores_except_id() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    let id = seed_user_with_email(&db, "self@example.com").await;
    TestContainer::singleton(db);

    let rule = Unique {
        table: "users",
        column: "email",
        except_id: Some(id),
    };
    assert!(rule.passes("self@example.com").await.is_ok());
}

#[test]
fn error_bag_scopes_default_and_named() {
    use suprnova::ValidationErrors;

    let mut errs = ValidationErrors::new();
    errs.add("email", "invalid");
    errs.add_to_bag("profile", "bio", "too long");
    errs.add_to_bag("profile", "avatar", "missing");

    // Bag-scoped errors are prefixed with bag name and a dot.
    // The default bag (added via `add`) stays unprefixed.
    assert!(errs.errors.contains_key("email"));
    assert!(errs.errors.contains_key("profile.bio"));
    assert!(errs.errors.contains_key("profile.avatar"));

    assert_eq!(errs.errors["email"][0], "invalid");
    assert_eq!(errs.errors["profile.bio"][0], "too long");
    assert_eq!(errs.errors["profile.avatar"][0], "missing");
}

// --- FormRequest::after_validation cross-field hook ---

mod after_validation_hook {
    use serde::Deserialize;
    use suprnova::{FormRequest, ValidationErrors};
    use validator::Validate;

    #[derive(Deserialize, Validate)]
    struct UpdatePassword {
        #[validate(length(min = 8))]
        #[allow(dead_code)]
        new_password: String,
        #[allow(dead_code)]
        confirmation: String,
    }

    impl FormRequest for UpdatePassword {
        fn after_validation(&self) -> Result<(), ValidationErrors> {
            if self.new_password != self.confirmation {
                let mut errs = ValidationErrors::new();
                errs.add("confirmation", "passwords do not match");
                return Err(errs);
            }
            Ok(())
        }
    }

    #[test]
    fn after_validation_runs_for_cross_field_checks() {
        let req = UpdatePassword {
            new_password: "longenough".into(),
            confirmation: "different".into(),
        };
        let result = req.after_validation();
        assert!(result.is_err());
        let errs = result.unwrap_err();
        assert!(errs.errors.contains_key("confirmation"));
    }

    #[test]
    fn after_validation_passes_when_fields_agree() {
        let req = UpdatePassword {
            new_password: "matching_password".into(),
            confirmation: "matching_password".into(),
        };
        assert!(req.after_validation().is_ok());
    }
}
