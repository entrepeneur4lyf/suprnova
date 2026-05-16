//! Avatar upload controller.
//!
//! Dogfoods the framework's new multipart + storage surface:
//!
//! - `#[derive(MultipartRequest)]` extracts the request body into a
//!   strongly-typed struct, running the `Image` magic-byte validator and
//!   the `MaxSize<N>` byte-boundary short-circuit during parsing.
//! - `Auth::user_as::<User>()` resolves the session-authenticated user
//!   through the registered `UserProvider`, so the storage path is
//!   derived entirely from server-controlled state (`user.id`).
//! - `Storage::disk("public").store_as(...)` writes the bytes to the
//!   public FS disk registered in `bootstrap::register_storage_disks`.
//!
//! Route is gated by `suprnova::AuthMiddleware::new()` in `routes.rs` so
//! the 401 path is owned by the middleware; the controller can assume
//! that `Auth::id()` is `Some` whenever it runs — but we still guard
//! against `Auth::user_as` returning `None` (e.g. session refers to a
//! user that was deleted between login and this request) to keep the
//! happy path explicit.

use suprnova::{
    handler, json_response, Auth, FrameworkError, Image, MaxSize, MultipartRequest, Response,
    Storage, UploadedFile,
};

use crate::models::users::User;

/// 5 MiB cap on avatar uploads — short-circuits oversize bodies at the
/// byte boundary without buffering the whole upload first.
const MAX_AVATAR_BYTES: usize = 5 * 1024 * 1024;

/// Multipart request body for the avatar upload endpoint.
///
/// Validators compose left-to-right: `Image` rejects non-image magic bytes
/// (422), `MaxSize` short-circuits past 5 MiB (413). Both run inside the
/// derived `FromRequest::from_request` impl before the handler body is
/// entered.
#[derive(MultipartRequest)]
pub struct AvatarUpload {
    #[field("avatar")]
    pub avatar: UploadedFile<(Image, MaxSize<MAX_AVATAR_BYTES>)>,
    #[field("caption")]
    pub caption: Option<String>,
}

#[handler]
pub async fn upload(form: AvatarUpload) -> Response {
    // `AuthMiddleware` already 401s on a guest, so reaching here implies
    // `Auth::id()` is `Some(...)`. We still match on `user_as` rather
    // than `expect()` because:
    // 1. The session might point at a user id that no longer exists in
    //    the DB (deleted account), and
    // 2. We want a clear 403 rather than a 500 panic in that edge case.
    let user = Auth::user_as::<User>()
        .await?
        .ok_or(FrameworkError::Unauthorized)?;

    // Path is derived ONLY from server-controlled state:
    // - `user.id` is a numeric primary key.
    // - The extension is sanitized to ASCII-alphanumeric with a length
    //   cap so a hostile filename like `../../etc/passwd` survives only
    //   as the fallback `"bin"`.
    //
    // The raw filename is never written into the storage path.
    let extension = form
        .avatar
        .file_name
        .as_deref()
        .and_then(|n| n.rsplit('.').next())
        .filter(|ext| {
            !ext.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric())
        })
        .unwrap_or("bin");
    let path = format!("avatars/{}.{extension}", user.id);

    let disk = Storage::disk("public")?;
    form.avatar.store_as(&disk, &path).await?;

    json_response!({
        "stored_at": path,
        "caption": form.caption,
    })
}
