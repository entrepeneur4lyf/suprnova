// Importing `post_policy` here causes the `inventory::submit!` blocks
// emitted by `#[policy(User, Post)]` to land in the binary, so
// `suprnova::authorization::init_policies()` can collect them.
pub mod post_policy;
