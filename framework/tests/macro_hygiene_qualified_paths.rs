//! Pin macro hygiene: `#[handler]`, `#[derive(FormRequest)]`, and the
//! `#[request]` attribute macro all emit fully-qualified `::suprnova::…`
//! paths. The historical bug — bare `suprnova::…` — broke any consumer
//! that vendored or renamed the framework crate AND any module that
//! happened to declare a local `mod suprnova { … }` that shadowed the
//! top-level path.
//!
//! This test compiles the macro emit inside a sibling
//! `mod suprnova { pub struct Stub; }` so a regression to bare-path
//! emission trips immediately at compile time: a bare
//! `suprnova::Request` lookup would find the local empty stub module
//! first and fail to resolve `Request`.

#![allow(dead_code)]

mod hygiene_inside_shadow {
    // The shadow that triggers the bug. If any macro under test emits
    // bare `suprnova::…`, name resolution walks into this empty module
    // first and produces "no such item" errors at macro expansion.
    mod suprnova {
        pub struct Stub;
    }

    use ::suprnova::FormRequestDerive;
    use ::suprnova::{HttpResponse, handler, json_response, model, request};

    // Eloquent macro — exercises `#[suprnova::model]` macro hygiene
    // (the table-side path emission). The macro emits inventory
    // entries and trait impls referencing ::suprnova::… across many
    // sites.
    #[model(table = "mh_users")]
    pub struct HygUser {
        pub id: i64,
        pub name: String,
    }

    // FormRequest derive — exercises the bare `suprnova::FormRequest`
    // emission this finding originally flagged. The macro also emits
    // the bare `serde::Deserialize` / `validator::Validate` derive
    // names in the `#[request]` attribute path; both are pulled
    // through the framework's re-exports below to keep the shadow
    // test self-contained.
    #[derive(
        ::suprnova::serde::Deserialize, ::suprnova::validator::Validate, FormRequestDerive,
    )]
    pub struct HygCreateUser {
        pub name: String,
    }

    // The `#[request]` attribute-macro emission (A1-M-002 site 2).
    #[request]
    pub struct HygUpdateUser {
        pub name: String,
    }

    // Handler — exercises bare `suprnova::Request` / `FrameworkError`
    // / `FromParam` / `AutoRouteBinding` / `FromRequest` emissions
    // (the seven sites the synthesis flagged). The `i64` parameter
    // routes through `FromParam`; the form-request parameter routes
    // through `FromRequest`.
    #[handler]
    pub async fn show(id: i64) -> Result<HttpResponse, ::suprnova::FrameworkError> {
        json_response!({ "id": id })
    }

    #[handler]
    pub async fn create(_req: HygCreateUser) -> Result<HttpResponse, ::suprnova::FrameworkError> {
        json_response!({ "ok": true })
    }
}

#[test]
fn macros_emit_fully_qualified_paths_under_shadowed_suprnova_mod() {
    // The compilation IS the assertion. If macros regressed to bare
    // `suprnova::…` paths, the `mod suprnova { pub struct Stub; }`
    // shadow above would resolve first and macro expansion would fail
    // with "could not find Request in suprnova" — this file would
    // refuse to compile and the test binary would never link.
}
