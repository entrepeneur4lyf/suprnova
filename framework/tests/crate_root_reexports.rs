//! Compile-time assertions that consumer-facing types reachable through
//! canonical APIs are re-exported at the crate root.
//!
//! Integration tests are separate crates and see only `pub` items from
//! `suprnova`; a name failing to resolve here is a regression of the
//! crate-root re-export law (`framework/CLAUDE.md` → "Crate-root re-export
//! law"). The bodies are empty on purpose — compilation is the assertion.

#![allow(unused_imports, dead_code)]

// L27 — `Storage::disk(name)` returns `opendal::Operator` and `DiskExt` is
// implemented on `opendal::Operator`. The full `opendal` crate must be
// reachable as `suprnova::opendal` so consumers never need to add `opendal`
// to their own Cargo.toml or risk a version-skew mismatch.
use suprnova::opendal;
use suprnova::opendal::Operator;
use suprnova::opendal::layers::{LoggingLayer, RetryLayer, TimeoutLayer};

// L28 — builder helpers for `AppConfig` / `ServerConfig` are the canonical
// way to construct those configs programmatically, so they belong at the
// root next to the value types they build.
use suprnova::{AppConfig, AppConfigBuilder, ServerConfig, ServerConfigBuilder};

// L28 — Inertia helper types referenced by the canonical surface
// (`Inertia::*`, the manifest pipeline, SSR, response/prop construction)
// must be reachable at the crate root, not buried under `suprnova::inertia::*`.
use suprnova::{
    IntoInertiaData, ManifestEntry, PropEntry, ResolvedAssets, SsrConfig, SsrResponse,
    ViteManifest,
};

#[test]
fn crate_root_reexports_resolve() {
    // The `use` statements above are the actual assertions: if any name
    // is removed from the public surface, this file fails to compile and
    // the integration-test job goes red. A no-op body keeps the test
    // harness happy without exercising runtime behaviour the imports
    // already prove.
    let _ = std::any::type_name::<Operator>();
    let _ = std::any::type_name::<AppConfigBuilder>();
    let _ = std::any::type_name::<ServerConfigBuilder>();
    let _ = std::any::type_name::<ViteManifest>();
    let _ = std::any::type_name::<SsrResponse>();
}
