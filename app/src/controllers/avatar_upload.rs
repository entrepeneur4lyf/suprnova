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
    Auth, FrameworkError, Image, MaxSize, MultipartRequest, Response, Storage, UploadedFile,
    handler, json_response,
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
    // - The extension comes from the file's MAGIC BYTES (via
    //   `UploadedFile::extension_from_magic`), not from the client-supplied
    //   filename. A request like `avatar=@evil.exe` where the body is real
    //   PNG bytes is stored as `avatars/<id>.png`, never `.exe`. Unknown
    //   content falls back to `"bin"`.
    //
    // The client filename is never used for path construction.
    let extension = form.avatar.extension_from_magic();
    let path = format!("avatars/{}.{extension}", user.id);

    let disk = Storage::disk("public")?;
    form.avatar.store_as(&disk, &path).await?;

    json_response!({
        "stored_at": path,
        "caption": form.caption,
    })
}
