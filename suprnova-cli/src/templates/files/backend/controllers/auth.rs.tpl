//! Authentication controller

use suprnova::{
    handler, inertia_response, redirect, serde_json, validator, Auth,
    FormRequest as FormRequestDerive, InertiaProps, Redirect, Request, Response, Validate,
};
use serde::Deserialize;

use crate::models::user::User;

// ============================================================================
// Login
// ============================================================================

#[derive(InertiaProps)]
pub struct LoginProps {
    pub errors: Option<serde_json::Value>,
}

#[handler]
pub async fn show_login(_req: Request) -> Response {
    inertia_response!("auth/Login", LoginProps { errors: None })
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

#[handler]
pub async fn login(req: Request) -> Response {
    let form: LoginRequest = req.input().await?;

    // Validate the form
    if let Err(errors) = form.validate() {
        return Ok(inertia_response!(
            "auth/Login",
            LoginProps {
                errors: Some(serde_json::json!(errors))
            }
        )?
        .status(422));
    }

    // Find user by email
    let user = match User::find_by_email(&form.email).await? {
        Some(u) => u,
        None => {
            return Ok(inertia_response!(
                "auth/Login",
                LoginProps {
                    errors: Some(serde_json::json!({
                        "email": ["These credentials do not match our records."]
                    }))
                }
            )?
            .status(422));
        }
    };

    // Verify password
    if !user.verify_password(&form.password)? {
        return Ok(inertia_response!(
            "auth/Login",
            LoginProps {
                errors: Some(serde_json::json!({
                    "email": ["These credentials do not match our records."]
                }))
            }
        )?
        .status(422));
    }

    // Log in the user
    Auth::login(user.id);

    // Handle remember me
    if form.remember {
        // Generate and store remember token
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
pub async fn show_register(_req: Request) -> Response {
    inertia_response!("auth/Register", RegisterProps { errors: None })
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

#[handler]
pub async fn register(req: Request) -> Response {
    let form: RegisterRequest = req.input().await?;

    // Validate the form
    if let Err(errors) = form.validate() {
        return Ok(inertia_response!(
            "auth/Register",
            RegisterProps {
                errors: Some(serde_json::json!(errors))
            }
        )?
        .status(422));
    }

    // Check password confirmation
    if form.password != form.password_confirmation {
        return Ok(inertia_response!(
            "auth/Register",
            RegisterProps {
                errors: Some(serde_json::json!({
                    "password_confirmation": ["Passwords do not match."]
                }))
            }
        )?
        .status(422));
    }

    // Check if email already exists
    if User::find_by_email(&form.email).await?.is_some() {
        return Ok(inertia_response!(
            "auth/Register",
            RegisterProps {
                errors: Some(serde_json::json!({
                    "email": ["This email is already registered."]
                }))
            }
        )?
        .status(422));
    }

    // Create user
    let user = User::create(&form.name, &form.email, &form.password).await?;

    // Log in the new user
    Auth::login(user.id);

    redirect!("/dashboard").into()
}

// ============================================================================
// Logout
// ============================================================================

#[handler]
pub async fn logout(_req: Request) -> Response {
    Auth::logout();
    redirect!("/").into()
}
