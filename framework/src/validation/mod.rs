//! Composable validation rules.
//!
//! This module provides the [`rule`] submodule containing rule-object
//! primitives — `Rule`, `ContextualRule`, `AsyncRule` — and the built-in
//! rules that ship with Suprnova (`Required`, `Email`, `Min`, `Max`,
//! `RequiredIf`, `RequiredWith`, `RequiredUnless`, `Unique`).
//!
//! Rule objects work alongside (and independently of) the `validator`
//! crate's `#[derive(Validate)]` flow — they are plain values that
//! implement the appropriate trait, so they can be composed,
//! stored, passed around, and applied to single field values.

pub mod rule;
