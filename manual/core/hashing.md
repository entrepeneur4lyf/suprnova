---
title: "Hashing"
description: "Password hashing with bcrypt, Argon2i, and Argon2id — Laravel-parity facade with env-driven driver selection"
icon: "key"
---

# Hashing

Suprnova ships a Laravel-parity hashing facade backed by three first-class drivers: **bcrypt** (default — matches Laravel), **Argon2i** (memory-hard, side-channel-resistant), and **Argon2id** (OWASP 2024 recommendation). Driver selection is env-driven; the facade is algorithm-aware end-to-end (info, is_hashed, needs_rehash, verify).

## Overview

```rust
use suprnova::hashing;

// Async (preferred inside Tokio request handlers — runs the CPU-bound
// hash on spawn_blocking so the worker thread stays free):
let hashed = hashing::hash_async("my_password").await?;
let valid = hashing::verify_async("my_password", &hashed).await?;

// Sync (tests, CLI tools, non-async contexts):
let hashed = hashing::hash("my_password")?;
let valid = hashing::verify("my_password", &hashed)?;
```

The free-function facade reads the active driver from `HASH_DRIVER` (or falls back to bcrypt). For explicit-driver calls, construct the driver type directly and pass it to `hash_with` / `verify_with` / `needs_rehash_with`.

## Configuration

| Variable | Description | Default | Range |
|----------|-------------|---------|-------|
| `HASH_DRIVER` | Active algorithm | `bcrypt` | `bcrypt` \| `argon` \| `argon2i` \| `argon2id` |
| `HASH_ROUNDS` | Bcrypt cost factor | `12` | `4..=31` (bcrypt only) |
| `HASH_MEMORY` | Argon memory cost in KiB | `65536` (64 MiB) | `>= 8` (argon only) |
| `HASH_TIME` | Argon time iterations | `4` | `>= 1` (argon only) |
| `HASH_THREADS` | Argon parallelism / lanes | `1` | `>= 1` (argon only) |
| `HASH_VERIFY` | When true, `verify()` rejects cross-algorithm hashes | `false` | `true` / `false` |

Misconfiguration (bad value, out-of-range parameter) surfaces as a `FrameworkError::param` at the first call to `hash` / `verify` / `needs_rehash` — not as a silent default.

### Example `.env` for argon2id

```env
HASH_DRIVER=argon2id
HASH_MEMORY=65536
HASH_TIME=4
HASH_THREADS=1
```

### Why Suprnova's Argon2 defaults are stronger than Laravel's

| Param | Laravel default | Suprnova default | Source |
|-------|-----------------|------------------|--------|
| Memory | 1 024 KiB (1 MiB) | 65 536 KiB (64 MiB) | OWASP 2024 |
| Time | 2 iterations | 4 iterations | OWASP 2024 |
| Threads | 2 | 1 | OWASP 2024 / libsodium-aligned |

Laravel's defaults assume PHP's request-per-process model — a worker can only spend so much on each password hash before the box is full. Tokio's `spawn_blocking` lets Suprnova hand the hash off to a blocking thread pool without freezing the request loop, so the OWASP 2024 numbers are realistic on real production hardware.

## Drivers

### Bcrypt (default)

```rust
use suprnova::hashing::{BcryptHasher, BcryptOptions, hash_with, verify_with};

let driver = BcryptHasher::new(BcryptOptions { rounds: 14 });
let hashed = hash_with(&driver, "my_password")?;
assert!(verify_with(&driver, "my_password", &hashed)?);
```

Bcrypt has a **72-byte block-size cap** on the password input — the underlying primitive silently truncates longer inputs, which means two distinct passphrases sharing their first 72 bytes hash to the same value. Suprnova rejects up-front (the framework's bcrypt path errors on `hash()` and returns `Ok(false)` on `verify()` for oversized passwords, keeping the auth flow's "invalid credentials" response uniform). Argon2 has no such ceiling.

The bcrypt cap is exposed as `suprnova::hashing::MAX_BCRYPT_PASSWORD_BYTES` (71 — the usable limit after the bcrypt null terminator).

### Argon2id (OWASP 2024 recommendation)

```rust
use suprnova::hashing::{Argon2idHasher, Argon2Options, hash_with, verify_with};

let driver = Argon2idHasher::new(Argon2Options {
    memory: 65_536,  // 64 MiB
    time: 4,
    threads: 1,
})?;

let hashed = hash_with(&driver, "my_password")?;
assert!(verify_with(&driver, "my_password", &hashed)?);

// Argon2 accepts arbitrary-length passphrases — the bcrypt 72-byte cap
// doesn't apply.
let long = "x".repeat(500);
let h = hash_with(&driver, &long)?;
assert!(verify_with(&driver, &long, &h)?);
```

### Argon2i

Same shape as Argon2id; `Argon2iHasher::new(opts)`. Use Argon2id for new projects — Argon2i is supported for parity but Argon2id is the modern recommendation.

## Algorithm-aware needs_rehash

`needs_rehash` returns `true` when the stored hash should be re-hashed under the active driver. It covers three cases:

1. **Algorithm mismatch** — bcrypt hash stored while `HASH_DRIVER=argon2id` (or vice versa). Triggers a rotation on next successful verify.
2. **Parameter weakness** — bcrypt cost below `HASH_ROUNDS`, or argon `m`/`t`/`p` below `HASH_MEMORY`/`HASH_TIME`/`HASH_THREADS`.
3. **Bcrypt legacy variants** — `$2a$`, `$2x$`, `$2y$` rotate to canonical `$2b$` even at the configured cost.

```rust
if hashing::needs_rehash(&stored_hash) {
    let fresh = hashing::hash_async("plaintext_at_login").await?;
    // Persist `fresh`. Standard Laravel "rehash on successful login"
    // pattern; works across algorithms.
}
```

Malformed input returns `true` — the caller naturally rotates anything it can't parse.

## Hash inspection (`info` + `is_hashed`)

```rust
use suprnova::hashing::{info, is_hashed};

let h = hashing::hash_async("my_password").await?;
let i = info(&h);
println!("algo: {}", i.algo.as_str());
println!("bcrypt cost: {:?}", i.rounds);
println!("argon memory KiB: {:?}", i.memory);

// True for any recognised algorithm hash; false for plaintext / garbage.
assert!(is_hashed(&h));
assert!(!is_hashed("plaintext"));
```

`info().algo` is one of: `Bcrypt`, `Argon2i`, `Argon2id`, `Argon2d` (recognised but never minted), `Unknown`.

`is_hashed` is what the `AsHashed` eloquent cast uses to skip re-hashing an already-hashed column — works across all three drivers, so flipping `HASH_DRIVER` mid-project doesn't cause a hash-of-hash loop on the next save.

## Cross-algorithm verification gate (`HASH_VERIFY`)

By default, `verify()` checks the password against the hash regardless of which algorithm produced the hash — this is what lets legacy bcrypt hashes still verify after you flip `HASH_DRIVER=argon2id` (so you can rotate them on login). Set `HASH_VERIFY=true` once every user is rotated to enforce the active algorithm strictly:

```env
HASH_VERIFY=true
```

With the gate on, `verify()` returns `Ok(false)` for any hash whose algorithm differs from the active driver — same shape as Laravel's `RuntimeException`, but Suprnova returns false rather than throwing because the auth-flow caller expects a `Result<bool>` either way.

## Async vs sync

Both bcrypt at cost 12 (~250 ms) and Argon2id at memory=64 MiB (~80 ms) are intentionally CPU-bound — that's the entire point of slow hashing. Calling the sync `hash` / `verify` directly from a Tokio request handler blocks the worker thread for the whole hash duration, starving other requests on the same worker.

Use the `*_async` siblings inside `async fn` handlers. They wrap the CPU-bound call in `tokio::task::spawn_blocking` so the worker stays free for other requests:

```rust
// GOOD — inside an async handler
let hashed = hashing::hash_async(&form.password).await?;

// BAD — blocks the worker for ~250 ms
let hashed = hashing::hash(&form.password)?;
```

The sync variants are for tests, CLI tools, and other non-async contexts where blocking is fine.

## Eloquent integration: `AsHashed` cast

The `#[cast(AsHashed)]` eloquent cast hashes a plaintext field on write using the active driver, and is **idempotent across all drivers** — saving a model whose `password` column already contains a recognised hash (bcrypt or argon) passes the value through unchanged. Without this guard, `User::find(id).await?.save().await?` would hash the existing hash on every save, breaking authentication.

```rust
use suprnova::eloquent::casts::AsHashed;

#[suprnova::model]
struct User {
    #[cast(AsHashed)]
    pub password: String,
    // ...
}
```

The idempotence check uses `hashing::is_hashed`, so flipping `HASH_DRIVER` mid-project is safe — both the legacy bcrypt hashes and the fresh argon2id hashes are recognised and skipped on re-save.

## Use with `Auth::attempt`

`Auth::attempt(&credentials)` calls `UserProvider::validate_credentials`, which in turn calls `hashing::verify_async` against the user's stored hash. This dispatches through the active driver, so the verify works regardless of which algorithm minted the stored hash — perfect for live migration: flip `HASH_DRIVER=argon2id`, all existing bcrypt hashes still verify, and `needs_rehash` returns `true` so you can rotate them on each successful login.

## See also

- [Authentication](authentication) — `Auth::attempt`, the user-provider trait, and how hashing integrates with login.
- [Auth flows](auth-flows) — password reset rotates the stored hash through `verify_and_rotate`, which uses the active driver.
- [Eloquent](eloquent) — `#[cast(AsHashed)]` reference.
