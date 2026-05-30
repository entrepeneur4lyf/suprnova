//! Regression tests for `#[form_request(custom_hooks)]`.
//!
//! Background: the `FormRequest` trait exposes three lifecycle hooks
//! (`authorize`, `after_validation`, `after_validation_async`) that
//! return defaults from the trait. Overriding any of them requires
//! writing your own `impl FormRequest`, but the `#[derive(FormRequest)]`
//! / `#[request]` macros also emit an `impl FormRequest` block — two
//! impls collide and the result fails to compile.
//!
//! `#[form_request(custom_hooks)]` is the opt-out that suppresses the
//! macro's emission so the caller can write their own. This mirrors the
//! existing `#[multipart(custom_hooks)]` shape on `MultipartRequest`.
//!
//! The hook bodies aren't exercised at runtime in this test (constructing
//! a `Request` outside hyper is non-trivial — see `data_form_request.rs`
//! for the full integration shape). Instead, this file's value is
//! compile-time: the fact that it builds at all proves there is exactly
//! one `impl FormRequest` per struct after macro expansion. A duplicate
//! impl would be a hard compile error.

use suprnova::{FormRequest, request};
use suprnova_macros::FormRequest as FormRequestDerive;

// ---- Sanity: default path (no `custom_hooks`) still works ----
//
// Without the opt-out the macro emits the default `impl FormRequest`.
// A user-written `impl FormRequest` here would duplicate — but there
// is none, so the file compiles and the default `max_body_bytes()`
// is reachable.

#[request]
pub struct PlainCreate {
    #[allow(dead_code)]
    pub email: String,
}

#[test]
fn plain_request_has_default_form_request_impl() {
    assert_eq!(
        <PlainCreate as FormRequest>::max_body_bytes(),
        suprnova::http::body::global_max_request_body_bytes(),
        "no `#[form_request(max_body_bytes = N)]` override → trait default kicks in"
    );
}

// ---- Opt-out via the `#[request]` attribute macro ----
//
// `#[form_request(custom_hooks)]` is parsed and consumed by `#[request]`
// so it doesn't leak through to the resulting struct (no derive declares
// it as a helper attribute). The default `impl FormRequest` is suppressed,
// and the user-written impl below stands alone.

#[request]
#[form_request(custom_hooks)]
pub struct RestrictedCreateAttr {
    #[allow(dead_code)]
    pub email: String,
}

impl FormRequest for RestrictedCreateAttr {
    fn max_body_bytes() -> usize {
        9_999
    }
}

#[test]
fn request_attr_with_custom_hooks_uses_user_impl() {
    assert_eq!(
        <RestrictedCreateAttr as FormRequest>::max_body_bytes(),
        9_999,
        "the user's `impl FormRequest` must be the only one — if the \
         macro had also emitted its default, this would be a duplicate-impl \
         error at compile time and we'd never reach this test"
    );
}

// ---- Opt-out via the `#[derive(FormRequest)]` derive form ----
//
// Same shape, but the user opts into Deserialize + Validate themselves.

#[derive(serde::Deserialize, validator::Validate, FormRequestDerive)]
#[form_request(custom_hooks)]
pub struct RestrictedCreateDerive {
    pub email: String,
}

impl FormRequest for RestrictedCreateDerive {
    fn max_body_bytes() -> usize {
        7_777
    }
}

#[test]
fn derive_with_custom_hooks_uses_user_impl() {
    assert_eq!(
        <RestrictedCreateDerive as FormRequest>::max_body_bytes(),
        7_777
    );
}

// ---- Compose `custom_hooks` with `max_body_bytes` under `#[request]` ----
//
// Opting out of hooks must NOT block the `max_body_bytes` override (the
// other supported `#[form_request(...)]` key). Both keys live on the
// same attribute; they must parse together.

#[request]
#[form_request(max_body_bytes = 1024)]
pub struct CapPlain {
    #[allow(dead_code)]
    pub data: String,
}

#[test]
fn max_body_bytes_under_default_hooks_applies() {
    assert_eq!(<CapPlain as FormRequest>::max_body_bytes(), 1024);
}

// With custom_hooks, the user writes the impl — so they own
// `max_body_bytes` themselves. The macro is silent on it.
