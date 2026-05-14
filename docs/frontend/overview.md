---
title: 'Frontend Overview'
description: 'Building modern frontends with Inertia.js and React'
icon: 'browser'
---

suprnova integrates [Inertia.js](https://inertiajs.com/) to provide a seamless way to build modern single-page applications using React, without the complexity of building an API.

## What is Inertia.js?

Inertia.js is a protocol that bridges your server-side framework with a client-side SPA framework. Instead of building a separate API:

- **Server** returns page components with their props
- **Client** renders React components using those props
- **Navigation** happens via XHR, updating the page without full reloads

This gives you the best of both worlds: server-side routing and controllers with a reactive frontend.

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                        Browser                               │
│  ┌───────────────────────────────────────────────────────┐  │
│  │                    React App                           │  │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐   │  │
│  │  │  Home.tsx   │  │  Users.tsx  │  │  Edit.tsx   │   │  │
│  │  └─────────────┘  └─────────────┘  └─────────────┘   │  │
│  │              ▲                                        │  │
│  │              │ Props (typed)                          │  │
│  │              │                                        │  │
│  │  ┌───────────────────────────────────────────────────┤  │
│  │  │              Inertia.js Adapter                    │  │
│  └──┴───────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                              │ HTTP (JSON for XHR / HTML for initial)
                              ▼
┌─────────────────────────────────────────────────────────────┐
│                      suprnova Backend                             │
│  ┌───────────────────────────────────────────────────────┐  │
│  │                    Controllers                         │  │
│  │     inertia_response!("Home", HomeProps { ... })      │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

## Project Structure

A suprnova application with Inertia has the following structure:

```
my-app/
├── src/
│   ├── main.rs
│   ├── routes.rs
│   ├── controllers/
│   │   └── home.rs          # Returns Inertia responses
│   └── models/
├── frontend/
│   ├── src/
│   │   ├── main.tsx         # Inertia app entry point
│   │   ├── pages/           # React page components
│   │   │   └── Home.tsx
│   │   └── types/
│   │       └── inertia-props.ts  # Generated TypeScript types
│   ├── package.json
│   └── vite.config.ts
└── .env
```

## How It Works

### 1. Initial Page Load

When a user visits your app for the first time:

1. Browser requests `/`
2. suprnova controller returns an HTML document containing:
   - The Inertia page data as JSON in the root element
   - Links to your compiled React app
3. React boots and renders the initial page component

### 2. Subsequent Navigation

When clicking links or submitting forms:

1. Inertia intercepts the navigation
2. Makes an XHR request with `X-Inertia: true` header
3. suprnova returns JSON with the new page component and props
4. Inertia swaps the component without a full page reload

## Getting Started

### Create a New Project

```bash
suprnova new my-app
cd my-app
```

This scaffolds a complete project with Inertia and React pre-configured.

### Start Development Server

```bash
suprnova serve
```

This starts both the Rust backend and Vite dev server with hot module replacement.

### Your First Inertia Response

In a controller, return an Inertia response:

```rust
use suprnova::{Request, Response, inertia_response};

#[derive(InertiaProps)]
pub struct HomeProps {
    pub title: String,
    pub message: String,
}

pub async fn index(_req: Request) -> Response {
    inertia_response!("Home", HomeProps {
        title: "Welcome".to_string(),
        message: "Hello from suprnova!".to_string(),
    })
}
```

Create the corresponding React component at `frontend/src/pages/Home.tsx`:

```tsx
import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message }: HomeProps) {
  return (
    <div>
      <h1>{title}</h1>
      <p>{message}</p>
    </div>
  )
}
```

## Development vs Production

### Development Mode

In development, suprnova:
- Serves React assets through Vite's dev server (`http://localhost:5173`)
- Enables hot module replacement for instant updates
- Provides detailed error messages

```env
# .env
INERTIA_DEVELOPMENT=true
```

### Production Mode

For production, build and serve optimized assets:

```bash
# Build frontend
cd frontend && npm run build

# Run production server
INERTIA_DEVELOPMENT=false suprnova serve --backend-only
```

## Key Benefits

| Feature | Benefit |
|---------|---------|
| **No API needed** | Controllers return props directly |
| **Type safety** | Generate TypeScript types from Rust |
| **Server-side routing** | Define routes once in Rust |
| **SPA experience** | No full page reloads |
| **SEO friendly** | Initial HTML is server-rendered |
| **Shared validation** | Use same validation rules everywhere |

## Next Steps

- [Inertia Responses](/frontend/inertia-responses) - Learn about the `inertia_response!` macro
- [Page Components](/frontend/pages) - Create React page components
- [TypeScript Types](/frontend/typescript-types) - Generate types from Rust structs
