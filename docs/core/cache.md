---
title: "Cache"
description: "Redis-backed caching with automatic in-memory fallback"
icon: "database"
---

# Cache

suprnova provides a Laravel-inspired Cache facade for storing and retrieving data with optional TTL (time-to-live) expiration. The cache automatically uses Redis when available, falling back to an in-memory cache when Redis is unavailable.

## Overview

The cache system is **automatically initialized** when your server starts - no manual setup required. It tries to connect to Redis first, and if Redis isn't available, it seamlessly falls back to an in-memory cache.

## Quick Start

```rust
use suprnova::Cache;
use std::time::Duration;

// Store a value with 1 hour TTL
Cache::put("user:1", &user, Some(Duration::from_secs(3600))).await?;

// Retrieve it
let user: Option<User> = Cache::get("user:1").await?;

// Check if exists
if Cache::has("user:1").await? {
    println!("User is cached!");
}

// Remove it
Cache::forget("user:1").await?;
```

## Configuration

The cache uses environment variables for configuration. All are optional with sensible defaults.

| Variable | Description | Default |
|----------|-------------|---------|
| `REDIS_URL` | Redis connection URL | `redis://127.0.0.1:6379` |
| `REDIS_PREFIX` | Prefix for all cache keys | `suprnova_cache:` |
| `CACHE_DEFAULT_TTL` | Default TTL in seconds (0 = no expiration) | `3600` |

### Example `.env`

```env
REDIS_URL=redis://localhost:6379
REDIS_PREFIX=myapp:cache:
CACHE_DEFAULT_TTL=7200
```

## Basic Operations

### Storing Items

```rust
use suprnova::Cache;
use std::time::Duration;

// Store with specific TTL
Cache::put("key", &value, Some(Duration::from_secs(3600))).await?;

// Store forever (no expiration)
Cache::forever("config:settings", &settings).await?;
```

### Retrieving Items

```rust
// Get a value (returns None if not found or expired)
let value: Option<MyType> = Cache::get("key").await?;

// Check if key exists
if Cache::has("key").await? {
    // Key exists and hasn't expired
}
```

### Removing Items

```rust
// Remove a single item
let was_removed = Cache::forget("key").await?;

// Clear all cached items
Cache::flush().await?;
```

## The Remember Pattern

The `remember` method retrieves an item from the cache, or stores a default value if it doesn't exist:

```rust
use suprnova::Cache;
use std::time::Duration;

// Get from cache, or compute and store if not cached
let user = Cache::remember("user:1", Some(Duration::from_secs(3600)), || async {
    // This only runs if "user:1" is not in cache
    User::find(1).await
}).await?;

// Store forever if not cached
let config = Cache::remember_forever("config:app", || async {
    load_config_from_database().await
}).await?;
```

This is perfect for expensive operations like database queries or API calls.

## Atomic Counters

Increment and decrement numeric values atomically:

```rust
use suprnova::Cache;

// Increment (creates key with 0 if doesn't exist)
let visits = Cache::increment("page:visits", 1).await?;

// Decrement
let remaining = Cache::decrement("quota:remaining", 1).await?;

// Increment by custom amount
let total = Cache::increment("stats:downloads", 10).await?;
```

## Testing

In tests, you can use the in-memory cache implementation directly:

```rust
use suprnova::{Cache, CacheStore, InMemoryCache};
use suprnova::container::testing::TestContainer;
use std::sync::Arc;

#[tokio::test]
async fn test_with_cache() {
    // Set up test container with in-memory cache
    let _guard = TestContainer::fake();
    TestContainer::bind::<dyn CacheStore>(Arc::new(InMemoryCache::new()));

    // Your test code - Cache operations will use InMemoryCache
    Cache::put("test:key", &"value", None).await.unwrap();

    let cached: Option<String> = Cache::get("test:key").await.unwrap();
    assert_eq!(cached, Some("value".to_string()));
}
```

## Type Safety

The cache works with any type that implements `Serialize` and `Deserialize`:

```rust
use serde::{Serialize, Deserialize};
use suprnova::Cache;

#[derive(Serialize, Deserialize)]
struct UserProfile {
    name: String,
    email: String,
    preferences: Vec<String>,
}

// Store complex types
let profile = UserProfile {
    name: "Alice".to_string(),
    email: "alice@example.com".to_string(),
    preferences: vec!["dark_mode".to_string()],
};

Cache::put("profile:1", &profile, None).await?;

// Retrieve with type inference
let cached: Option<UserProfile> = Cache::get("profile:1").await?;
```

## Redis vs In-Memory

| Feature | Redis | In-Memory |
|---------|-------|-----------|
| Persistence | Yes (if configured) | No |
| Shared across processes | Yes | No |
| TTL support | Yes | Yes |
| Atomic operations | Yes | Yes |
| Default | When `REDIS_URL` is accessible | Fallback |

The framework automatically selects the appropriate backend:
- **Redis**: When `REDIS_URL` environment variable is set and Redis is accessible
- **In-Memory**: When Redis is not available (development, testing, or Redis failure)

## Best Practices

### Key Naming

Use consistent, hierarchical key names:

```rust
// Good
Cache::put("users:1:profile", &profile, None).await?;
Cache::put("posts:123:comments:count", &count, None).await?;

// Avoid
Cache::put("user_profile_1", &profile, None).await?;
```

### TTL Strategy

Choose TTL based on data volatility:

```rust
use std::time::Duration;

// Frequently changing data - short TTL
Cache::put("stats:active_users", &count, Some(Duration::from_secs(60))).await?;

// Semi-static data - longer TTL
Cache::put("config:features", &features, Some(Duration::from_secs(3600))).await?;

// Static data - no expiration
Cache::forever("translations:en", &translations).await?;
```

### Cache Invalidation

Invalidate related cache entries when data changes:

```rust
async fn update_user(id: i32, data: UserUpdate) -> Result<User, Error> {
    let user = User::update(id, data).await?;

    // Invalidate related caches
    Cache::forget(&format!("users:{}:profile", id)).await?;
    Cache::forget(&format!("users:{}:permissions", id)).await?;

    Ok(user)
}
```
