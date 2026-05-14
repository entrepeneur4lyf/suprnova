---
title: 'TypeScript Types'
description: 'Generate TypeScript types from your Rust props structs'
icon: 'code'
---

suprnova can automatically generate TypeScript interfaces from your Rust `InertiaProps` structs, ensuring type safety between your backend and frontend.

## Generating Types

Run the generate-types command:

```bash
suprnova generate-types
```

This scans your Rust source files for `#[derive(InertiaProps)]` structs and generates TypeScript interfaces in `frontend/src/types/inertia-props.ts`.

## How It Works

Given this Rust code:

```rust
// src/controllers/home.rs
use suprnova::InertiaProps;
use serde::Serialize;

#[derive(Serialize)]
pub struct User {
    pub id: i32,
    pub name: String,
    pub email: String,
}

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub message: String,
    pub user: User,
    pub count: i32,
    pub tags: Vec<String>,
    pub metadata: Option<String>,
}
```

suprnova generates:

```typescript
// frontend/src/types/inertia-props.ts
export interface User {
  id: number
  name: string
  email: string
}

export interface HomeProps {
  title: string
  message: string
  user: User
  count: number
  tags: string[]
  metadata: string | null
}
```

## Type Mappings

suprnova converts Rust types to TypeScript equivalents:

| Rust Type | TypeScript Type |
|-----------|-----------------|
| `String`, `&str` | `string` |
| `i8`, `i16`, `i32`, `i64` | `number` |
| `u8`, `u16`, `u32`, `u64` | `number` |
| `f32`, `f64` | `number` |
| `bool` | `boolean` |
| `Option<T>` | `T \| null` |
| `Vec<T>` | `T[]` |
| `HashMap<K, V>` | `Record<K, V>` |
| Custom structs | Generated interface |

## Using Generated Types

Import types in your React components:

```tsx
// frontend/src/pages/Home.tsx
import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message, user, tags }: HomeProps) {
  return (
    <div>
      <h1>{title}</h1>
      <p>{message}</p>

      <div>
        <h2>Welcome, {user.name}</h2>
        <p>Email: {user.email}</p>
      </div>

      <ul>
        {tags.map((tag) => (
          <li key={tag}>{tag}</li>
        ))}
      </ul>
    </div>
  )
}
```

## Nested Structs

Nested structs are automatically included:

```rust
#[derive(Serialize)]
pub struct Address {
    pub street: String,
    pub city: String,
    pub zip: String,
}

#[derive(Serialize)]
pub struct Company {
    pub name: String,
    pub address: Address,
}

#[derive(InertiaProps)]
pub struct ProfileProps {
    pub user_name: String,
    pub company: Company,
}
```

Generates:

```typescript
export interface Address {
  street: string
  city: string
  zip: string
}

export interface Company {
  name: string
  address: Address
}

export interface ProfileProps {
  user_name: string
  company: Company
}
```

## Best Practices

### Run After Schema Changes

Regenerate types whenever you modify props:

```bash
# After changing Rust props
suprnova generate-types
```

### Add to Build Process

Include type generation in your development workflow:

```bash
# In package.json scripts
{
  "scripts": {
    "dev": "suprnova generate-types && vite",
    "build": "suprnova generate-types && vite build"
  }
}
```

### Use Strict Mode

Enable strict TypeScript checking in `tsconfig.json`:

```json
{
  "compilerOptions": {
    "strict": true,
    "noUncheckedIndexedAccess": true
  }
}
```

### Handle Optional Values

Always handle `null` cases for `Option<T>` fields:

```tsx
// Rust: pub avatar_url: Option<String>
// TypeScript: avatar_url: string | null

function UserAvatar({ avatar_url }: { avatar_url: string | null }) {
  if (!avatar_url) {
    return <DefaultAvatar />
  }
  return <img src={avatar_url} alt="Avatar" />
}
```

## Workflow Integration

### Development Workflow

1. Define props in Rust controller
2. Run `suprnova generate-types`
3. Import types in React component
4. Get full autocomplete and type checking

### Example Workflow

```rust
// 1. Define props in Rust
#[derive(InertiaProps)]
pub struct TodoListProps {
    pub todos: Vec<Todo>,
    pub filter: String,
    pub total_count: i32,
}
```

```bash
# 2. Generate types
suprnova generate-types
```

```tsx
// 3. Use in React with full type safety
import type { TodoListProps } from '../types/inertia-props'

export default function TodoList({ todos, filter, total_count }: TodoListProps) {
  // Full autocomplete for todos, filter, total_count
  return (
    <div>
      <h1>Todos ({total_count})</h1>
      <p>Filter: {filter}</p>
      <ul>
        {todos.map((todo) => (
          // Full autocomplete for todo.id, todo.title, etc.
          <li key={todo.id}>{todo.title}</li>
        ))}
      </ul>
    </div>
  )
}
```

## FormRequest Types for Type-Safe Forms

FormRequests can also generate TypeScript types, enabling end-to-end type safety for form submissions.

### Generating FormRequest Types

Add `#[derive(InertiaProps)]` to your FormRequest structs:

```rust
use suprnova::{request, InertiaProps};

#[request]
#[derive(InertiaProps)]
pub struct CreateUserRequest {
    #[validate(email(message = "Invalid email"))]
    pub email: String,

    #[validate(length(min = 8, message = "Password must be at least 8 characters"))]
    pub password: String,

    #[validate(length(min = 1, max = 100))]
    pub name: String,
}
```

Run `suprnova generate-types` to generate:

```typescript
export interface CreateUserRequest {
  email: string
  password: string
  name: string
}
```

### Type-Safe Forms with Inertia

Use the generated type with Inertia's `<Form>` component for the cleanest approach:

```tsx
import { Form, usePage } from '@inertiajs/react'

export default function Register() {
  const { errors } = usePage().props

  return (
    <Form action="/register" method="post">
      <input type="email" name="email" />
      {errors?.email && <span>{errors.email}</span>}

      <input type="password" name="password" />
      {errors?.password && <span>{errors.password}</span>}

      <input type="text" name="name" />
      {errors?.name && <span>{errors.name}</span>}

      <button type="submit">Register</button>
    </Form>
  )
}
```

For more control over form state, use `useForm` with the generated types:

```tsx
import { useForm } from '@inertiajs/react'
import type { CreateUserRequest } from '../types/inertia-props'

export default function Register() {
  const { data, setData, post, processing, errors } = useForm<CreateUserRequest>({
    email: '',
    password: '',
    name: '',
  })

  return (
    <Form action="/register" method="post">
      {({ processing }) => (
        <>
          <input
            type="email"
            name="email"
            value={data.email}
            onChange={(e) => setData('email', e.target.value)}
          />
          {errors.email && <span>{errors.email}</span>}

          <input
            type="password"
            name="password"
            value={data.password}
            onChange={(e) => setData('password', e.target.value)}
          />
          {errors.password && <span>{errors.password}</span>}

          <input
            type="text"
            name="name"
            value={data.name}
            onChange={(e) => setData('name', e.target.value)}
          />
          {errors.name && <span>{errors.name}</span>}

          <button type="submit" disabled={processing}>
            Register
          </button>
        </>
      )}
    </Form>
  )
}
```

### Benefits

- **Field name autocomplete**: TypeScript suggests valid field names
- **Type checking**: Catch type mismatches at compile time
- **Validation alignment**: TypeScript types match Rust validation rules
- **Error handling**: The `errors` object has matching field keys

> **Note:**
>
> For more information on requests and validation, see [Requests](/core/requests).


## Type-Safe Routes (Inertia v2+)

suprnova generates type-safe route helpers that work natively with Inertia.js v2+ `UrlMethodPair` interface. This enables fully type-safe navigation without manually typing URLs or HTTP methods.

### Generated Output

Running `suprnova generate-types` also generates `frontend/src/types/routes.ts`:

```typescript
// frontend/src/types/routes.ts
import type { Method } from '@inertiajs/core';

export interface RouteConfig<TData = void> {
  url: string;
  method: Method;  // 'get' | 'post' | 'put' | 'patch' | 'delete'
  data?: TData;
}

// Path parameter types
export interface UserShowParams {
  id: string;
}

// Controller namespace - mirrors backend structure
export const controllers = {
  home: {
    index: (): RouteConfig => ({ url: '/', method: 'get' }),
  },
  user: {
    index: (): RouteConfig => ({ url: '/users', method: 'get' }),
    show: (params: UserShowParams): RouteConfig => ({
      url: `/users/${params.id}`,
      method: 'get',
    }),
    store: (): RouteConfig => ({ url: '/users', method: 'post' }),
  },
  todo: {
    list: (): RouteConfig => ({ url: '/todos', method: 'get' }),
    create_random: (): RouteConfig => ({ url: '/todos/random', method: 'post' }),
  },
} as const;

// Named routes lookup
export const routes = {
  'home': controllers.home.index,
  'users.index': controllers.user.index,
  'users.show': controllers.user.show,
} as const;
```

### Usage with Inertia.js

The generated `controllers` object works directly with Inertia v2's native APIs:

```tsx
import { router, useForm, Link } from '@inertiajs/react';
import { controllers } from '@/types/routes';

// Navigation with router.visit()
router.visit(controllers.home.index());
router.visit(controllers.user.show({ id: '123' }));

// Form submission with useForm
const form = useForm({ title: '', completed: false });
form.submit(controllers.todo.create_random());

// Link component
<Link href={controllers.user.show({ id: '123' })}>View User</Link>
<Link href={controllers.todo.list()}>All Todos</Link>
```

### Path Parameters

Routes with path parameters like `/users/{id}` automatically generate typed parameter objects:

```rust
// Backend route registration
get!("/users/{id}", controllers::user::show).name("users.show")
```

```typescript
// Generated TypeScript
export interface UserShowParams {
  id: string;
}

export const controllers = {
  user: {
    show: (params: UserShowParams): RouteConfig => ({
      url: `/users/${params.id}`,
      method: 'get',
    }),
  },
} as const;
```

```tsx
// Usage - TypeScript ensures you pass the correct params
controllers.user.show({ id: '123' })  // OK
controllers.user.show()               // Error: missing params
controllers.user.show({ name: 'a' })  // Error: wrong property
```

### How Route Scanning Works

suprnova scans your `src/routes.rs` file and extracts:

1. **HTTP method**: `get`, `post`, `put`, `patch`, `delete`
2. **Path**: Including path parameters like `{id}`
3. **Handler**: Module and function name (e.g., `controllers::user::show`)
4. **Route name**: Optional `.name("route.name")` suffix

```rust
// src/routes.rs
use suprnova::{get, post, routes};

routes! {
    get!("/", controllers::home::index).name("home"),
    get!("/users", controllers::user::index).name("users.index"),
    get!("/users/{id}", controllers::user::show).name("users.show"),
    post!("/users", controllers::user::store).name("users.store"),
    get!("/todos", controllers::todo::list).name("todos.index"),
    post!("/todos/random", controllers::todo::create_random),
}
```

### Named Routes Lookup

Named routes are exported as a lookup object for easy access:

```typescript
import { routes, controllers } from '@/types/routes';

// Using named routes
router.visit(routes['home']());
router.visit(routes['users.show']({ id: '123' }));

// Same as using controllers directly
router.visit(controllers.home.index());
router.visit(controllers.user.show({ id: '123' }));
```

### Benefits

| Benefit | Description |
|---------|-------------|
| Type-safe URLs | No more typos in route paths |
| Type-safe methods | HTTP method is always correct |
| Type-safe params | Path parameters are validated |
| IDE autocomplete | Full IntelliSense support |
| Native Inertia support | Works directly with v2+ APIs |

## Troubleshooting

### Types Not Updating

If types aren't reflecting your changes:

1. Ensure structs have `#[derive(InertiaProps)]`
2. Run `suprnova generate-types` again
3. Restart your TypeScript language server

### Missing Nested Types

For nested structs to be included, they must be:
- Used in an `InertiaProps` struct
- Have `#[derive(Serialize)]`

### Type Errors

If you get type mismatches:
- Check that Rust and TypeScript types align
- Verify `Option<T>` handling (generates `T | null`)
- Ensure vectors generate arrays (`Vec<T>` → `T[]`)

## Summary

| Command | Description |
|---------|-------------|
| `suprnova generate-types` | Generate TypeScript from Rust props and routes |

### Generated Files

| File | Contents |
|------|----------|
| `frontend/src/types/inertia-props.ts` | TypeScript interfaces from `#[derive(InertiaProps)]` structs |
| `frontend/src/types/routes.ts` | Type-safe route helpers from `src/routes.rs` |

### Features

| Feature | Benefit |
|---------|---------|
| Automatic generation | No manual type definitions |
| Type safety | Catch errors at compile time |
| Autocomplete | Full IDE support |
| Nested types | Complex structures supported |
| Type-safe routes | URL and method safety with Inertia v2+ |
| Path parameters | Typed parameter objects for dynamic routes |
