//! Built-in console commands shipped by the framework.
//!
//! Each builtin uses `#[command]` to submit a [`CommandEntry`] into
//! the global inventory registry. Linking the framework into a project
//! pulls the builtins along automatically — there is no opt-in step.
//!
//! [`CommandEntry`]: crate::console::CommandEntry

pub mod db_seed;
