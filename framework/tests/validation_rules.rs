//! Integration tests for the rule-object primitives in
//! `suprnova::validation::rule`.

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement, Value};
use suprnova::rules::{
    Alpha, AlphaNum, Between, Boolean, Confirmed, Different, Email, In, Integer, Max, Min, NotIn,
    Numeric, Required, RequiredIf, RequiredUnless, RequiredWith, Same, Url, Uuid,
};
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

// --- expanded sync rule set ---

#[test]
fn between_checks_inclusive_length_range() {
    let r = Between(3, 5);
    assert!(r.passes("abc").is_ok());
    assert!(r.passes("abcde").is_ok());
    assert!(r.passes("ab").is_err());
    assert!(r.passes("abcdef").is_err());
}

#[test]
fn in_and_not_in_check_membership() {
    let allowed = In(&["red", "green", "blue"]);
    assert!(allowed.passes("red").is_ok());
    assert!(allowed.passes("yellow").is_err());

    let banned = NotIn(&["admin", "root"]);
    assert!(banned.passes("user").is_ok());
    assert!(banned.passes("admin").is_err());
}

#[test]
fn integer_and_numeric_parse_correctly() {
    assert!(Integer.passes("42").is_ok());
    assert!(Integer.passes("-17").is_ok());
    assert!(Integer.passes("3.14").is_err());
    assert!(Integer.passes("abc").is_err());

    assert!(Numeric.passes("3.14").is_ok());
    assert!(Numeric.passes("42").is_ok());
    assert!(Numeric.passes("-1.5e10").is_ok());
    assert!(Numeric.passes("abc").is_err());
}

#[test]
fn boolean_accepts_common_truthy_falsy() {
    for v in &[
        "true", "false", "TRUE", "False", "0", "1", "yes", "no", "ON", "off",
    ] {
        assert!(Boolean.passes(v).is_ok(), "should accept: {v}");
    }
    assert!(Boolean.passes("maybe").is_err());
    assert!(Boolean.passes("2").is_err());
}

#[test]
fn alpha_and_alphanum_check_character_classes() {
    assert!(Alpha.passes("hello").is_ok());
    assert!(
        Alpha.passes("héllo").is_ok(),
        "unicode alphabetic accepted"
    );
    assert!(Alpha.passes("hello42").is_err());
    assert!(Alpha.passes("").is_err());

    assert!(AlphaNum.passes("hello42").is_ok());
    assert!(AlphaNum.passes("user_name-42").is_ok());
    assert!(AlphaNum.passes("user@name").is_err());
    assert!(AlphaNum.passes("").is_err());
}

#[test]
fn url_and_uuid_validate_format() {
    assert!(Url.passes("https://example.com").is_ok());
    assert!(Url.passes("http://x.test/p?q=1").is_ok());
    assert!(Url.passes("not a url").is_err());

    assert!(Uuid.passes("550e8400-e29b-41d4-a716-446655440000").is_ok());
    assert!(Uuid.passes("not-a-uuid").is_err());
}

// --- expanded contextual rule set ---

#[test]
fn same_checks_field_equality() {
    let rule = Same { other: "password" };
    let c = ctx(&[("password", "secret")]);
    assert!(rule.passes("secret", &c).is_ok());
    assert!(rule.passes("different", &c).is_err());
    let c2 = ctx(&[]);
    assert!(
        rule.passes("anything", &c2).is_err(),
        "missing other field → fail"
    );
}

#[test]
fn different_rejects_equality() {
    let rule = Different {
        other: "old_password",
    };
    let c = ctx(&[("old_password", "secret")]);
    assert!(rule.passes("new_secret", &c).is_ok());
    assert!(rule.passes("secret", &c).is_err());
}

#[test]
fn confirmed_matches_field_confirmation_suffix() {
    let rule = Confirmed { field: "password" };
    let c = ctx(&[("password_confirmation", "secret")]);
    assert!(rule.passes("secret", &c).is_ok());
    assert!(rule.passes("different", &c).is_err());
    let c2 = ctx(&[]);
    assert!(
        rule.passes("anything", &c2).is_err(),
        "missing confirmation → fail"
    );
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

// --- ValidationErrors::into_result ---

#[test]
fn validation_errors_into_result_returns_ok_when_empty() {
    use suprnova::ValidationErrors;
    let empty = ValidationErrors::new();
    assert!(empty.into_result().is_ok());
}

#[test]
fn validation_errors_into_result_returns_err_when_populated() {
    use suprnova::ValidationErrors;
    let mut errs = ValidationErrors::new();
    errs.add("field", "msg");
    let result = errs.into_result();
    assert!(result.is_err());
    let bag = result.unwrap_err();
    assert!(bag.errors.contains_key("field"));
}

// --- validate! macro ---

mod validate_macro {
    use suprnova::rules::{Email, Min, Required, RequiredIf};
    use suprnova::{validate, ValidationErrors};

    struct UserForm {
        email: String,
        name: String,
    }

    #[test]
    fn validate_macro_passes_when_all_rules_succeed() {
        let form = UserForm {
            email: "shawn@example.com".into(),
            name: "Shawn".into(),
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            email => Required, Email;
            name => Required, Min(2);
        };
        assert!(result.is_ok());
    }

    #[test]
    fn validate_macro_accumulates_failures_across_fields() {
        let form = UserForm {
            email: "not-an-email".into(),
            name: "".into(),
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            email => Required, Email;
            name => Required, Min(2);
        };
        let errs = result.unwrap_err();
        assert!(errs.errors.contains_key("email"), "email error missing");
        assert!(errs.errors.contains_key("name"), "name error missing");
        // Email field gets ONE error: Email fails; Required passes since
        // "not-an-email" is a non-empty string.
        assert_eq!(errs.errors["email"].len(), 1);
        // Name field gets TWO errors: Required AND Min(2) both fail.
        assert_eq!(errs.errors["name"].len(), 2);
    }

    #[test]
    fn validate_macro_runs_contextual_rules_with_ctx_suffix() {
        use std::collections::HashMap;

        struct BillingForm {
            billing_type: String,
            card_number: String,
        }

        let form = BillingForm {
            billing_type: "card".into(),
            card_number: "".into(),
        };

        let mut ctx: HashMap<String, String> = HashMap::new();
        ctx.insert("billing_type".to_string(), "card".to_string());

        let result: Result<(), ValidationErrors> = validate! { form =>
            billing_type => Required;
            card_number => RequiredIf {
                other: "billing_type",
                value: "card",
            } => with ctx;
        };
        let errs = result.unwrap_err();
        assert!(errs.errors.contains_key("card_number"));
    }

    #[test]
    fn validate_macro_with_trailing_semicolon_is_accepted() {
        let form = UserForm {
            email: "x@example.com".into(),
            name: "ok".into(),
        };
        // Demonstrates the `$(;)?` tail in the macro matcher — having
        // a trailing `;` after the last field row must not error.
        let result: Result<(), ValidationErrors> = validate! { form =>
            email => Required, Email;
            name => Required;
        };
        assert!(result.is_ok());
    }
}

// --- AsyncRule::check_async helper ---

#[tokio::test]
async fn async_rule_check_helper_accumulates_errors() {
    use suprnova::ValidationErrors;

    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    seed_user_with_email(&db, "taken@example.com").await;
    TestContainer::singleton(db);

    let mut errs = ValidationErrors::new();
    Unique {
        table: "users",
        column: "email",
        except_id: None,
    }
    .check_async("taken@example.com", &mut errs, "email")
    .await;
    assert!(
        errs.errors.contains_key("email"),
        "duplicate email should produce an `email` error"
    );
}

#[tokio::test]
async fn async_rule_check_helper_leaves_empty_on_success() {
    use suprnova::ValidationErrors;

    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    TestContainer::singleton(db);

    let mut errs = ValidationErrors::new();
    Unique {
        table: "users",
        column: "email",
        except_id: None,
    }
    .check_async("nobody@example.com", &mut errs, "email")
    .await;
    assert!(errs.is_empty(), "no rows → no errors");
}
