//! `#[accessor]` and `#[mutator]` — function-level attribute macros that
//! tag methods on a `#[suprnova::model]` `impl` block as Eloquent
//! attribute readers / writers.
//!
//! Both macros are pass-throughs at the proc-macro level: they emit the
//! user's `fn` unchanged. The wiring is name-based — the struct-level
//! `#[model(appends = [...], mutators = [...])]` arrays drive the
//! `to_json` / `fill` emission in `derive_eloquent.rs`. The function
//! attributes survive in the source as documentation and a discovery
//! hook for tooling, mirroring Laravel's intent-revealing
//! `protected function getFullNameAttribute()` convention.
//!
//! ## Accessor contract
//!
//! ```rust,ignore
//! impl User {
//!     #[suprnova::accessor]
//!     pub fn full_name(&self) -> String { ... }
//! }
//! ```
//!
//! `to_json()` calls `self.full_name()` and inserts the JSON-encoded
//! result under the key `"full_name"` when the field is listed in
//! `appends = [...]` on the `#[model]` attribute.
//!
//! ## Mutator contract
//!
//! ```rust,ignore
//! impl User {
//!     #[suprnova::mutator]
//!     pub fn set_password(
//!         &mut self,
//!         value: ::serde_json::Value,
//!     ) -> Result<(), suprnova::FrameworkError> { ... }
//! }
//! ```
//!
//! When the field is listed in `mutators = [...]`, the model's `fill`
//! path calls `self.set_<field>(value)?` instead of doing direct
//! `self.<field> = serde_json::from_value(value)`. The user's body owns
//! the deserialise + transform — keeping the macro typing-agnostic
//! across `String`, `i32`, custom enums, etc.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{ImplItemFn, Result, parse2};

/// `#[accessor]` — marks `fn name(&self) -> T` as a readable Eloquent
/// attribute. Emits the function unchanged; the model macro's
/// `to_json` emission picks the method up by name from
/// `appends = [...]`.
pub fn accessor(_attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let func: ImplItemFn = parse2(item)?;
    Ok(quote! { #func })
}

/// `#[mutator]` — marks `fn set_<field>(&mut self, value: serde_json::Value)
/// -> Result<(), FrameworkError>` as the routed write-path for `<field>`.
/// Emits the function unchanged; the model macro's `fill` emission
/// dispatches to it when the field is listed in `mutators = [...]`.
///
/// The signature isn't enforced here. The macro-generated `fill` body
/// calls `self.set_<field>(value.clone())?` — any signature divergence
/// surfaces as a clear compiler error at that call site
/// ("expected `serde_json::Value`, found `String`" or similar).
pub fn mutator(_attr: TokenStream, item: TokenStream) -> Result<TokenStream> {
    let func: ImplItemFn = parse2(item)?;
    Ok(quote! { #func })
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn accessor_passes_function_through_unchanged() {
        let out = accessor(
            quote! {},
            quote! { pub fn full_name(&self) -> String { String::new() } },
        )
        .expect("accessor parses");
        // Ensure the function signature survives in the output.
        let s = out.to_string();
        assert!(s.contains("full_name"));
        assert!(s.contains("String"));
    }

    #[test]
    fn mutator_passes_function_through_unchanged() {
        let out = mutator(
            quote! {},
            quote! {
                pub fn set_password(
                    &mut self,
                    value: ::serde_json::Value,
                ) -> Result<(), ()> { Ok(()) }
            },
        )
        .expect("mutator parses");
        let s = out.to_string();
        assert!(s.contains("set_password"));
        assert!(s.contains("Value"));
    }
}
