//! Authentication controller.
//!
//! Renders the login/register Inertia pages on GET, validates and
//! persists credentials on POST, redirects to `/dashboard` on success.
//! Form bodies are extracted via `FormRequest`, which means per-field
//! validation errors come back as a standard 422 with the Laravel-style
//! `{ message, errors }` envelope — the Inertia client surfaces those
//! automatically on the originating page.

use serde::Deserialize;
use suprnova::{
    handler, inertia_response, redirect, serde_json, Auth, FormRequest, InertiaProps, Request,
    Response, Validate, ValidationErrors,
};

use crate::models::user::User;

// ============================================================================
// Login
// ============================================================================

#[derive(InertiaProps)]
pub struct LoginProps {
    /// Errors carried over from the redirect-back flow. The Inertia
    /// client merges any session-flashed errors into `errors` on its
    /// own; this prop exists so the page can render before any
    /// submission too.
    pub errors: Option<serde_json::Value>,
}

#[handler]
pub async fn show_login(req: Request) -> Response {
    inertia_response!(&req, "auth/Login", LoginProps { errors: None })
}

#[derive(Deserialize, Validate)]
pub struct LoginRequest {
    #[validate(email(message = "Please enter a valid email address"))]
    pub email: String,
    #[validate(length(min = 1, message = "Password is required"))]
    pub password: String,
    #[serde(default)]
    pub remember: bool,
}

impl FormRequest for LoginRequest {}

/// Build a `FrameworkError::Validation` that pins the failure to the
/// `email` field, mirroring how the bundled validators surface errors.
fn invalid_credentials() -> suprnova::FrameworkError {
    let mut errs = ValidationErrors::new();
    errs.add("email", "These credentials do not match our records.");
    suprnova::FrameworkError::Validation(errs)
}

#[handler]
pub async fn login(form: LoginRequest) -> Response {
    let user = User::find_by_email(&form.email)
        .await?
        .ok_or_else(invalid_credentials)?;

    if !user.verify_password(&form.password)? {
        return Err(invalid_credentials().into());
    }

    // Log in the user. `Auth::login_id` takes the user id as a string so
    // the session layer stays type-agnostic.
    Auth::login_id(user.id.to_string());

    if form.remember {
        // Persist a remember token so the auth provider can resume the
        // session from a cookie after the in-memory session expires.
        let token = suprnova::session::generate_session_id();
        user.update_remember_token(Some(token)).await?;
    }

    redirect!("/dashboard").into()
}

// ============================================================================
// Registration
// ============================================================================

#[derive(InertiaProps)]
pub struct RegisterProps {
    pub errors: Option<serde_json::Value>,
}

#[handler]
pub async fn show_register(req: Request) -> Response {
    inertia_response!(&req, "auth/Register", RegisterProps { errors: None })
}

#[derive(Deserialize, Validate)]
pub struct RegisterRequest {
    #[validate(length(min = 2, message = "Name must be at least 2 characters"))]
    pub name: String,
    #[validate(email(message = "Please enter a valid email address"))]
    pub email: String,
    #[validate(length(min = 8, message = "Password must be at least 8 characters"))]
    pub password: String,
    pub password_confirmation: String,
}

impl FormRequest for RegisterRequest {
    /// Cross-field check: confirm the password and its confirmation
    /// match. Runs after the per-field rules pass, so we know each
    /// individual value is well-formed before comparing them.
    fn after_validation(&self) -> Result<(), ValidationErrors> {
        if self.password != self.password_confirmation {
            let mut errs = ValidationErrors::new();
            errs.add("password_confirmation", "Passwords do not match.");
            return Err(errs);
        }
        Ok(())
    }
}

#[handler]
pub async fn register(form: RegisterRequest) -> Response {
    if User::find_by_email(&form.email).await?.is_some() {
        let mut errs = ValidationErrors::new();
        errs.add("email", "This email is already registered.");
        return Err(suprnova::FrameworkError::Validation(errs).into());
    }

    let user = User::create(&form.name, &form.email, &form.password).await?;
    Auth::login_id(user.id.to_string());

    redirect!("/dashboard").into()
}

// ============================================================================
// Logout
// ============================================================================

#[handler]
pub async fn logout(_req: Request) -> Response {
    Auth::logout().await?;
    redirect!("/").into()
}
