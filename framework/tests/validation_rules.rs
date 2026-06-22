//! Integration tests for the rule-object primitives in
//! `suprnova::validation::rule`.

use sea_orm::{ConnectionTrait, Database, DbBackend, Statement, Value};
use suprnova::rules::{
    Alpha, AlphaDash, AlphaNum, Between, Boolean, Confirmed, Different, Email, HttpUrl, In,
    Integer, Max, Min, NotIn, Numeric, Required, RequiredIf, RequiredUnless, RequiredWith, Same,
    Url, Uuid,
};
use suprnova::testing::TestContainer;
use suprnova::{AsyncRule, ContextualRule, DbConnection, FormContext, Rule, Unique};

#[test]
fn required_passes_on_present() {
    let r = Required;
    assert!(r.passes("not empty").is_ok());
    assert!(r.passes("").is_err());
    assert!(r.passes("   ").is_err(), "all-whitespace counts as empty");
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
        others: &["address_line_1"],
    };
    let c = ctx(&[("address_line_1", "1 Main St")]);
    assert!(rule.passes("12345", &c).is_ok());
    assert!(rule.passes("", &c).is_err());

    let c2 = ctx(&[]);
    assert!(rule.passes("", &c2).is_ok());
}

#[test]
fn required_with_triggers_when_any_other_field_present() {
    // Laravel `required_with:foo,bar` — at least one must be present.
    use suprnova::ContextualRule;
    let rule = RequiredWith {
        others: &["address_line_1", "address_line_2"],
    };
    // Only the second sibling is present — still triggers.
    let c = ctx(&[("address_line_2", "Apt 7")]);
    assert!(rule.passes("", &c).is_err());
    // Neither present — passes.
    let c2 = ctx(&[]);
    assert!(rule.passes("", &c2).is_ok());
}

#[test]
fn required_with_all_only_triggers_when_every_sibling_present() {
    use suprnova::ContextualRule;
    use suprnova::RequiredWithAll;
    let rule = RequiredWithAll {
        others: &["address_line_1", "city"],
    };
    // Only one of the two — does NOT trigger.
    let c = ctx(&[("address_line_1", "1 Main St")]);
    assert!(rule.passes("", &c).is_ok());
    // Both present — value is required.
    let c2 = ctx(&[("address_line_1", "1 Main St"), ("city", "Springfield")]);
    assert!(rule.passes("", &c2).is_err());
    assert!(rule.passes("12345", &c2).is_ok());
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

    let rule = Unique::new("users", "email");
    assert!(rule.passes("nobody@example.com").await.is_ok());
}

#[tokio::test]
async fn unique_fails_when_row_exists() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    seed_user_with_email(&db, "taken@example.com").await;
    TestContainer::singleton(db);

    let rule = Unique::new("users", "email");
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

    let rule = Unique::new("users", "email").ignore(id);
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
    assert!(Alpha.passes("héllo").is_ok(), "unicode alphabetic accepted");
    assert!(Alpha.passes("hello42").is_err());
    assert!(Alpha.passes("").is_err());

    assert!(AlphaNum.passes("hello42").is_ok());
    // AlphaNum (Laravel `alpha_num`) is letters + digits only — separators
    // belong to AlphaDash.
    assert!(AlphaNum.passes("user_name-42").is_err());
    assert!(AlphaNum.passes("user@name").is_err());
    assert!(AlphaNum.passes("").is_err());

    // AlphaDash (Laravel `alpha_dash`) also allows '-' and '_', but not
    // spaces or other punctuation.
    assert!(AlphaDash.passes("user_name-42").is_ok());
    assert!(AlphaDash.passes("user name").is_err());
    assert!(AlphaDash.passes("user@name").is_err());
    assert!(AlphaDash.passes("").is_err());
}

#[test]
fn url_and_uuid_validate_format() {
    assert!(Url.passes("https://example.com").is_ok());
    assert!(Url.passes("http://x.test/p?q=1").is_ok());
    assert!(Url.passes("not a url").is_err());

    assert!(Uuid.passes("550e8400-e29b-41d4-a716-446655440000").is_ok());
    assert!(Uuid.passes("not-a-uuid").is_err());
}

#[test]
fn numeric_rejects_non_finite() {
    // Rust's f64 parser accepts these, but they're not valid user-input
    // numbers.
    assert!(Numeric.passes("NaN").is_err());
    assert!(Numeric.passes("inf").is_err());
    assert!(Numeric.passes("-inf").is_err());
    assert!(Numeric.passes("infinity").is_err());
    assert!(
        Numeric.passes("1e400").is_err(),
        "a magnitude that overflows to infinity must be rejected"
    );
    // Finite values still pass.
    assert!(Numeric.passes("3.14").is_ok());
    assert!(Numeric.passes("-42").is_ok());
}

#[test]
fn http_url_requires_http_scheme() {
    assert!(HttpUrl.passes("https://example.com").is_ok());
    assert!(HttpUrl.passes("http://x.test/p?q=1").is_ok());
    // Schemes that plain `Url` accepts but `HttpUrl` rejects.
    assert!(HttpUrl.passes("file:///etc/passwd").is_err());
    assert!(HttpUrl.passes("javascript:alert(1)").is_err());
    assert!(HttpUrl.passes("ftp://host/file").is_err());
    assert!(HttpUrl.passes("not a url").is_err());
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
fn confirmed_check_named_matches_field_confirmation_suffix() {
    use suprnova::ValidationErrors;

    // Production path: `Confirmed` is a unit struct and gets the field
    // name via `check_named`. The `validate!` macro threads
    // `stringify!($field)` automatically; for direct use, callers pass
    // the field name themselves.
    let rule = Confirmed;
    let c = ctx(&[("password_confirmation", "secret")]);

    // Matching confirmation → no error added.
    let mut errs = ValidationErrors::new();
    rule.check_named("secret", &mut errs, "password", &c);
    assert!(errs.is_empty(), "matching confirmation must not error");

    // Mismatching value → error added.
    let mut errs = ValidationErrors::new();
    rule.check_named("different", &mut errs, "password", &c);
    assert!(
        errs.errors.contains_key("password"),
        "mismatching confirmation must error on the field key"
    );

    // Missing confirmation field → error added.
    let mut errs = ValidationErrors::new();
    rule.check_named("anything", &mut errs, "password", &ctx(&[]));
    assert!(
        errs.errors.contains_key("password"),
        "missing confirmation field must error"
    );
}

#[test]
fn confirmed_passes_returns_helpful_error_without_field_name() {
    // `passes` doesn't get the field name, so it can't look up
    // `<field>_confirmation`. Calling it directly must return an
    // explanatory `Err` rather than silently passing or matching the
    // wrong value.
    let rule = Confirmed;
    let c = ctx(&[("password_confirmation", "secret")]);
    let err = rule.passes("secret", &c).unwrap_err();
    assert!(
        err.contains("check_named") || err.contains("validate!") || err.contains("field name"),
        "passes should explain how to use Confirmed correctly, got: {err}"
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
    use suprnova::rules::{Email, Max, Min, Required, RequiredIf};
    use suprnova::{ValidationErrors, validate};

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

    // --- `?:` Optional-field marker ---
    //
    // `validate! { self => bio?: Min(10); }` runs `Min(10)` only when
    // `self.bio` is `Some`. The macro expands to
    // `if let Some(ref __val) = self.bio { ... }` so `None` is a no-op
    // and a populated `Some` runs every rule on the row.

    struct OptionalForm {
        email: Option<String>,
        bio: Option<String>,
    }

    #[test]
    fn validate_macro_skips_optional_field_when_none() {
        let form = OptionalForm {
            email: None,
            bio: None,
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            email?: Required, Email;
            bio?: Min(10);
        };
        assert!(
            result.is_ok(),
            "None values must skip validation entirely, got: {:?}",
            result.err().map(|e| e.errors)
        );
    }

    #[test]
    fn validate_macro_runs_rules_on_some_optional_field() {
        let form = OptionalForm {
            email: Some("not-an-email".into()),
            bio: None,
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            email?: Email;
            bio?: Min(10);
        };
        let errs = result.unwrap_err();
        assert!(
            errs.errors.contains_key("email"),
            "email should fail Email validation; got bag: {:?}",
            errs.errors
        );
        assert!(
            !errs.errors.contains_key("bio"),
            "bio is None, no rule should run"
        );
    }

    #[test]
    fn validate_macro_mixes_required_and_optional_rows() {
        struct MixedForm {
            email: String,
            bio: Option<String>,
        }
        let form = MixedForm {
            email: "shawn@example.com".into(),
            bio: Some("hi".into()), // too short for Min(5)
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            email => Required, Email;
            bio?: Min(5);
        };
        let errs = result.unwrap_err();
        assert!(
            errs.errors.contains_key("bio"),
            "bio = Some(\"hi\") should fail Min(5)"
        );
        assert!(
            !errs.errors.contains_key("email"),
            "email is valid; should not appear"
        );
    }

    #[test]
    fn validate_macro_optional_row_with_multiple_rules() {
        // Both rules on the row should run for a populated Option.
        let form = OptionalForm {
            email: None,
            bio: Some("x".into()), // fails Min(10), passes Max(500)
        };
        let result: Result<(), ValidationErrors> = validate! { form =>
            bio?: Min(10), Max(500);
        };
        let errs = result.unwrap_err();
        assert!(errs.errors.contains_key("bio"));
        assert_eq!(
            errs.errors["bio"].len(),
            1,
            "only Min should fail; got {:?}",
            errs.errors["bio"]
        );
    }

    // --- Confirmed as a unit struct via `validate!` ---
    //
    // The `validate!` macro now dispatches `ContextualRule::check_named`
    // and threads `stringify!($field)` into the rule. `Confirmed`
    // overrides `check_named` to derive `<field>_confirmation` from the
    // ident, so callers write `password => Confirmed => with ctx;` —
    // the field name appears once, not twice.

    use suprnova::Confirmed;

    struct PasswordForm {
        password: String,
    }

    #[test]
    fn validate_macro_confirmed_unit_struct_passes_when_confirmation_matches() {
        use std::collections::HashMap;
        let form = PasswordForm {
            password: "secret".into(),
        };
        let mut ctx: HashMap<String, String> = HashMap::new();
        ctx.insert("password_confirmation".to_string(), "secret".to_string());

        let result: Result<(), ValidationErrors> = validate! { form =>
            password => Confirmed => with ctx;
        };
        assert!(
            result.is_ok(),
            "matching confirmation must pass; got: {:?}",
            result.err().map(|e| e.errors)
        );
    }

    #[test]
    fn validate_macro_confirmed_unit_struct_fails_when_confirmation_mismatches() {
        use std::collections::HashMap;
        let form = PasswordForm {
            password: "secret".into(),
        };
        let mut ctx: HashMap<String, String> = HashMap::new();
        ctx.insert("password_confirmation".to_string(), "different".to_string());

        let result: Result<(), ValidationErrors> = validate! { form =>
            password => Confirmed => with ctx;
        };
        let errs = result.unwrap_err();
        assert!(
            errs.errors.contains_key("password"),
            "mismatching confirmation must produce a `password` error; got: {:?}",
            errs.errors
        );
    }

    // --- `?=>` conditional-presence row (RequiredIf on an absent Option) ---

    struct CheckoutForm {
        billing_type: String,
        card_number: Option<String>,
    }

    fn billing_ctx(billing_type: &str) -> std::collections::HashMap<String, String> {
        let mut ctx = std::collections::HashMap::new();
        ctx.insert("billing_type".to_string(), billing_type.to_string());
        ctx
    }

    #[test]
    fn validate_macro_conditional_required_fires_on_absent_optional() {
        // card_number is None, but billing_type == "card" requires it.
        // `?:` would silently skip; `?=>` evaluates and fails.
        let form = CheckoutForm {
            billing_type: "card".into(),
            card_number: None,
        };
        let ctx = billing_ctx(&form.billing_type);
        let result: Result<(), ValidationErrors> = validate! { form =>
            card_number ?=> RequiredIf { other: "billing_type", value: "card" } => with ctx;
        };
        let errs = result.unwrap_err();
        assert!(
            errs.errors.contains_key("card_number"),
            "RequiredIf must fire on an absent Option when the condition holds; got {:?}",
            errs.errors
        );
    }

    #[test]
    fn validate_macro_conditional_required_passes_when_condition_absent() {
        // billing_type != "card" → card_number not required even though None.
        let form = CheckoutForm {
            billing_type: "invoice".into(),
            card_number: None,
        };
        let ctx = billing_ctx(&form.billing_type);
        let result: Result<(), ValidationErrors> = validate! { form =>
            card_number ?=> RequiredIf { other: "billing_type", value: "card" } => with ctx;
        };
        assert!(
            result.is_ok(),
            "absent optional must pass when the condition does not hold; got {:?}",
            result.err().map(|e| e.errors)
        );
    }

    #[test]
    fn validate_macro_conditional_required_evaluates_present_value() {
        // `?=>` also runs on a populated Some value (here it passes).
        let form = CheckoutForm {
            billing_type: "card".into(),
            card_number: Some("4111111111111111".into()),
        };
        let ctx = billing_ctx(&form.billing_type);
        let result: Result<(), ValidationErrors> = validate! { form =>
            card_number ?=> RequiredIf { other: "billing_type", value: "card" } => with ctx;
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
    Unique::new("users", "email")
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
    Unique::new("users", "email")
        .check_async("nobody@example.com", &mut errs, "email")
        .await;
    assert!(errs.is_empty(), "no rows → no errors");
}

// --- FrameworkError::from_unique_violation — map DB constraint to 422 ---
//
// `Unique` is advisory (TOCTOU); the DB UNIQUE constraint is the real
// guarantee. These tests prove the write-error → 422 mapping closes the
// loop the loser of a race hits, and that non-unique errors pass through
// as 500-class errors rather than being misreported as validation.

async fn db_with_unique_email() -> DbConnection {
    let raw = Database::connect("sqlite::memory:").await.unwrap();
    raw.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE accounts (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL UNIQUE)"
            .to_string(),
    ))
    .await
    .unwrap();
    DbConnection::from_raw(raw)
}

#[tokio::test]
async fn from_unique_violation_maps_duplicate_insert_to_422() {
    use suprnova::FrameworkError;

    let db = db_with_unique_email().await;
    let backend = db.inner().get_database_backend();
    let insert = |email: &str| {
        Statement::from_sql_and_values(
            backend,
            "INSERT INTO accounts (email) VALUES (?)",
            vec![Value::from(email.to_string())],
        )
    };

    // First insert succeeds; the second violates the UNIQUE constraint.
    db.inner().execute(insert("dup@example.com")).await.unwrap();
    let dup_err = db
        .inner()
        .execute(insert("dup@example.com"))
        .await
        .unwrap_err();

    let mapped = FrameworkError::from_unique_violation("email", "Email already taken", dup_err);
    assert!(
        matches!(mapped, FrameworkError::Validation(_)),
        "a unique-constraint violation must map to a Validation (422) error"
    );
    assert_eq!(mapped.status_code(), 422);
    match &mapped {
        FrameworkError::Validation(errs) => {
            assert_eq!(errs.errors["email"][0], "Email already taken");
        }
        other => panic!("expected Validation, got {other:?}"),
    }
}

#[tokio::test]
async fn from_unique_violation_passes_through_non_unique_errors() {
    use suprnova::FrameworkError;

    let db = db_with_unique_email().await;
    let backend = db.inner().get_database_backend();
    // Write to a table that doesn't exist — a real DbErr that is NOT a
    // unique-constraint violation.
    let err = db
        .inner()
        .execute(Statement::from_string(
            backend,
            "INSERT INTO does_not_exist (x) VALUES (1)".to_string(),
        ))
        .await
        .unwrap_err();

    let mapped = FrameworkError::from_unique_violation("email", "ignored", err);
    assert!(
        !matches!(mapped, FrameworkError::Validation(_)),
        "a non-unique DB error must pass through, not become a 422"
    );
    assert_ne!(mapped.status_code(), 422);
}

// --- Unique builder options ---

#[tokio::test]
async fn unique_where_eq_scopes_the_check() {
    let _guard = TestContainer::fake();
    let raw = Database::connect("sqlite::memory:").await.unwrap();
    raw.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE members (id INTEGER PRIMARY KEY AUTOINCREMENT, email TEXT NOT NULL, \
         tenant_id INTEGER NOT NULL)"
            .to_string(),
    ))
    .await
    .unwrap();
    raw.execute(Statement::from_sql_and_values(
        DbBackend::Sqlite,
        "INSERT INTO members (email, tenant_id) VALUES (?, ?)",
        vec![Value::from("a@x.com".to_string()), Value::from(1i64)],
    ))
    .await
    .unwrap();
    TestContainer::singleton(DbConnection::from_raw(raw));

    // Same email, same tenant → taken.
    assert!(
        Unique::new("members", "email")
            .where_eq("tenant_id", 1i64)
            .passes("a@x.com")
            .await
            .is_err()
    );
    // Same email, different tenant → free (scoped out by the predicate).
    assert!(
        Unique::new("members", "email")
            .where_eq("tenant_id", 2i64)
            .passes("a@x.com")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn unique_case_insensitive_folds_case() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    seed_user_with_email(&db, "foo@example.com").await;
    TestContainer::singleton(db);

    // Differently-cased value collides only with case_insensitive().
    assert!(
        Unique::new("users", "email")
            .case_insensitive()
            .passes("FOO@EXAMPLE.COM")
            .await
            .is_err(),
        "LOWER() comparison must treat FOO@EXAMPLE.COM as the stored foo@example.com"
    );
    // Default (case-sensitive) comparison must NOT collide on different case.
    assert!(
        Unique::new("users", "email")
            .passes("FOO@EXAMPLE.COM")
            .await
            .is_ok()
    );
}

#[tokio::test]
async fn unique_ignore_with_column_excludes_by_custom_key() {
    let _guard = TestContainer::fake();
    let raw = Database::connect("sqlite::memory:").await.unwrap();
    raw.execute(Statement::from_string(
        DbBackend::Sqlite,
        "CREATE TABLE widgets (widget_id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT NOT NULL)"
            .to_string(),
    ))
    .await
    .unwrap();
    let res = raw
        .execute(Statement::from_sql_and_values(
            DbBackend::Sqlite,
            "INSERT INTO widgets (name) VALUES (?)",
            vec![Value::from("gizmo".to_string())],
        ))
        .await
        .unwrap();
    let id = res.last_insert_id() as i64;
    TestContainer::singleton(DbConnection::from_raw(raw));

    // Excluding the only matching row by its custom PK column → free.
    assert!(
        Unique::new("widgets", "name")
            .ignore_with_column("widget_id", id)
            .passes("gizmo")
            .await
            .is_ok()
    );
    // Not excluding → taken.
    assert!(
        Unique::new("widgets", "name")
            .passes("gizmo")
            .await
            .is_err()
    );
}

#[tokio::test]
async fn unique_rejects_malformed_identifiers() {
    let _guard = TestContainer::fake();
    let db = fresh_db().await;
    TestContainer::singleton(db);

    // A hostile/typo'd table name errors at the identifier gate, before
    // any SQL is built or run.
    let err = Unique::new("users; DROP TABLE users", "email")
        .passes("x@y.com")
        .await
        .unwrap_err();
    assert!(
        err.contains("identifier"),
        "expected an identifier-validation error, got: {err}"
    );
}

// --- Unique through FormRequest::after_validation_async (the recipe) ---
//
// Proves the documented integration: an app that overrides
// `after_validation_async` to call `Unique::check_async` gets automatic,
// framework-invoked uniqueness validation. Driven directly (not through a
// socket) so the thread-local test DB is visible to the rule.

mod unique_via_form_request {
    use super::*;
    use suprnova::{FormRequest, ValidationErrors};

    #[derive(serde::Deserialize, validator::Validate)]
    struct RegisterForm {
        #[validate(email)]
        email: String,
    }

    #[suprnova::async_trait]
    impl FormRequest for RegisterForm {
        async fn after_validation_async(&self) -> Result<(), ValidationErrors> {
            let mut errs = ValidationErrors::new();
            Unique::new("users", "email")
                .check_async(&self.email, &mut errs, "email")
                .await;
            errs.into_result()
        }
    }

    #[tokio::test]
    async fn after_validation_async_runs_unique_and_rejects_duplicate() {
        let _guard = TestContainer::fake();
        let db = fresh_db().await;
        seed_user_with_email(&db, "taken@example.com").await;
        TestContainer::singleton(db);

        let form = RegisterForm {
            email: "taken@example.com".into(),
        };
        let errs = form.after_validation_async().await.unwrap_err();
        assert!(errs.errors.contains_key("email"));
    }

    #[tokio::test]
    async fn after_validation_async_passes_for_free_value() {
        let _guard = TestContainer::fake();
        let db = fresh_db().await;
        TestContainer::singleton(db);

        let form = RegisterForm {
            email: "free@example.com".into(),
        };
        assert!(form.after_validation_async().await.is_ok());
    }
}
