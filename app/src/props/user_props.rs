//! `UserProps` — the canonical user DTO for the Suprnova example app.
//!
//! Demonstrates the "one struct, both ends" pattern enabled by
//! `#[derive(Data)]`:
//!
//! - **Outbound** (Inertia pagination): `UserProps` is the element type of
//!   `CursorPaginator<UserProps>` passed to `Inertia::paginate(...)`.
//!   The `Serialize` impl emitted by `Data` satisfies the
//!   `T: serde::Serialize + 'static` bound on `Inertia::paginate`.
//!
//! - **Inbound** (FormRequest): `UserProps::extract(req).await` deserialises
//!   and validates a JSON request body.  The `Deserialize` + `FormRequest`
//!   impls are also emitted by `Data`.
//!
//! Validation rules:
//! - `email`  — must be a well-formed e-mail address.
//! - `name`   — must be at least one character long.

use suprnova::Data;
use validator::Validate;

#[derive(Debug, Clone, Data, Validate)]
pub struct UserProps {
    pub id: i64,

    #[validate(email)]
    pub email: String,

    #[validate(length(min = 1))]
    pub name: String,
}

#[cfg(test)]
mod tests {
    use super::UserProps;

    /// Prove that `#[derive(Data)]` generates a working `Deserialize`
    /// impl: a valid JSON payload round-trips through `serde_json`.
    #[test]
    fn deserializes_valid_payload() {
        let json = serde_json::json!({
            "id": 1,
            "email": "ada@example.com",
            "name": "Ada"
        });

        let user: UserProps = serde_json::from_value(json).expect("should deserialize");
        assert_eq!(user.id, 1);
        assert_eq!(user.email, "ada@example.com");
        assert_eq!(user.name, "Ada");
    }

    /// Prove that `#[derive(Data)]` generates a working `Serialize` impl:
    /// the struct serialises back to JSON with all fields present.
    #[test]
    fn serializes_all_fields() {
        let user = UserProps {
            id: 42,
            email: "grace@example.com".to_string(),
            name: "Grace".to_string(),
        };

        let v = serde_json::to_value(&user).expect("should serialize");
        assert_eq!(v["id"], 42);
        assert_eq!(v["email"], "grace@example.com");
        assert_eq!(v["name"], "Grace");
    }

    /// Prove validation works: missing/empty name fails `length(min = 1)`.
    #[test]
    fn validation_rejects_empty_name() {
        use validator::Validate;

        let user = UserProps {
            id: 1,
            email: "valid@example.com".to_string(),
            name: String::new(),
        };
        assert!(user.validate().is_err(), "empty name should fail validation");
    }

    /// Prove validation works: malformed e-mail fails `#[validate(email)]`.
    #[test]
    fn validation_rejects_bad_email() {
        use validator::Validate;

        let user = UserProps {
            id: 1,
            email: "not-an-email".to_string(),
            name: "Ada".to_string(),
        };
        assert!(
            user.validate().is_err(),
            "bad email should fail validation"
        );
    }
}
