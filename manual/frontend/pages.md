---
title: 'Page Components'
description: 'Creating React page components for Inertia'
icon: 'file-code'
---

Page components are React components that receive props from your suprnova controllers. They live in `frontend/src/pages/` and are automatically resolved by Inertia.

## Creating a Page Component

Page components are standard React components that receive typed props:

```tsx
// frontend/src/pages/Home.tsx
import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message }: HomeProps) {
  return (
    <div className="p-8">
      <h1 className="text-3xl font-bold">{title}</h1>
      <p className="mt-4">{message}</p>
    </div>
  )
}
```

## Directory Structure

Organize pages to match your route structure:

```
frontend/src/pages/
├── Home.tsx              # inertia_response!("Home", ...)
├── About.tsx             # inertia_response!("About", ...)
├── Users/
│   ├── Index.tsx         # inertia_response!("Users/Index", ...)
│   ├── Show.tsx          # inertia_response!("Users/Show", ...)
│   └── Edit.tsx          # inertia_response!("Users/Edit", ...)
├── Posts/
│   ├── Index.tsx         # inertia_response!("Posts/Index", ...)
│   └── Show.tsx          # inertia_response!("Posts/Show", ...)
└── Admin/
    └── Dashboard.tsx     # inertia_response!("Admin/Dashboard", ...)
```

## Using the CLI Generator

Generate page components with the CLI:

```bash
suprnova make:page Home
```

This creates a page component with the correct structure:

```tsx
// frontend/src/pages/Home.tsx
import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message }: HomeProps) {
  return (
    <div className="font-sans p-8 max-w-xl mx-auto">
      <h1 className="text-3xl font-bold text-gray-900">{title}</h1>
      <p className="mt-2 text-gray-600">{message}</p>
    </div>
  )
}
```

## Page Component Patterns

### List Page

Display a collection of items:

```tsx
// frontend/src/pages/Posts/Index.tsx
import type { PostsIndexProps } from '../../types/inertia-props'
import { Link } from '@inertiajs/react'

export default function PostsIndex({ posts, total }: PostsIndexProps) {
  return (
    <div className="p-8">
      <div className="flex justify-between items-center mb-6">
        <h1 className="text-2xl font-bold">Posts ({total})</h1>
        <Link
          href="/posts/create"
          className="bg-blue-500 text-white px-4 py-2 rounded"
        >
          New Post
        </Link>
      </div>

      <ul className="space-y-4">
        {posts.map((post) => (
          <li key={post.id} className="border p-4 rounded">
            <Link href={`/posts/${post.id}`} className="text-lg font-medium">
              {post.title}
            </Link>
            <p className="text-gray-600 mt-1">{post.excerpt}</p>
          </li>
        ))}
      </ul>
    </div>
  )
}
```

### Detail Page

Show a single item:

```tsx
// frontend/src/pages/Posts/Show.tsx
import type { PostShowProps } from '../../types/inertia-props'
import { Link } from '@inertiajs/react'

export default function PostShow({ post, author }: PostShowProps) {
  return (
    <article className="p-8 max-w-2xl mx-auto">
      <Link href="/posts" className="text-blue-500 mb-4 inline-block">
        &larr; Back to posts
      </Link>

      <h1 className="text-3xl font-bold mt-4">{post.title}</h1>

      <p className="text-gray-500 mt-2">
        By {author.name} on {post.created_at}
      </p>

      <div className="mt-6 prose">
        {post.content}
      </div>
    </article>
  )
}
```

### Form Page

Create or edit items using Inertia's `<Form>` component:

```tsx
// frontend/src/pages/Posts/Create.tsx
import type { PostCreateProps } from '../../types/inertia-props'
import { Form, usePage } from '@inertiajs/react'

export default function PostCreate({ categories }: PostCreateProps) {
  const { errors } = usePage().props

  return (
    <div className="p-8 max-w-xl mx-auto">
      <h1 className="text-2xl font-bold mb-6">Create Post</h1>

      <Form action="/posts" method="post" className="space-y-4">
        {({ processing }) => (
          <>
            <div>
              <label className="block text-sm font-medium mb-1">Title</label>
              <input
                type="text"
                name="title"
                className="w-full border rounded px-3 py-2"
              />
              {errors?.title && (
                <p className="text-red-500 text-sm mt-1">{errors.title}</p>
              )}
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">Category</label>
              <select
                name="category_id"
                className="w-full border rounded px-3 py-2"
              >
                <option value="">Select a category</option>
                {categories.map((cat) => (
                  <option key={cat.id} value={cat.id}>{cat.name}</option>
                ))}
              </select>
            </div>

            <div>
              <label className="block text-sm font-medium mb-1">Content</label>
              <textarea
                name="content"
                rows={6}
                className="w-full border rounded px-3 py-2"
              />
            </div>

            <button
              type="submit"
              disabled={processing}
              className="bg-blue-500 text-white px-4 py-2 rounded disabled:opacity-50"
            >
              {processing ? 'Saving...' : 'Create Post'}
            </button>
          </>
        )}
      </Form>
    </div>
  )
}
```

## Navigation with Inertia

Use Inertia's `Link` component for SPA-style navigation:

```tsx
import { Link } from '@inertiajs/react'

// Basic link
<Link href="/posts">Posts</Link>

// With method
<Link href={`/posts/${id}`} method="delete" as="button">
  Delete
</Link>

// Preserve scroll position
<Link href="/posts" preserveScroll>Posts</Link>

// Replace history entry
<Link href="/posts" replace>Posts</Link>
```

## Programmatic Navigation

Navigate programmatically using the `router`:

```tsx
import { router } from '@inertiajs/react'

// Visit a page
router.visit('/posts')

// With options
router.visit('/posts', {
  method: 'post',
  data: { title: 'New Post' },
  preserveScroll: true,
})

// POST request
router.post('/posts', { title: 'New Post' })

// PUT request
router.put(`/posts/${id}`, { title: 'Updated' })

// DELETE request
router.delete(`/posts/${id}`)

// Reload current page
router.reload()
```

## Shared Data

Access shared data that's available on every page:

```tsx
import { usePage } from '@inertiajs/react'

export default function Layout({ children }) {
  const { auth } = usePage().props

  return (
    <div>
      <nav>
        {auth.user ? (
          <span>Welcome, {auth.user.name}</span>
        ) : (
          <Link href="/login">Login</Link>
        )}
      </nav>
      <main>{children}</main>
    </div>
  )
}
```

## Layouts

Create reusable layouts for your pages:

```tsx
// frontend/src/layouts/AppLayout.tsx
import { Link } from '@inertiajs/react'

interface Props {
  children: React.ReactNode
}

export default function AppLayout({ children }: Props) {
  return (
    <div className="min-h-screen bg-gray-100">
      <nav className="bg-white shadow p-4">
        <div className="max-w-6xl mx-auto flex gap-4">
          <Link href="/">Home</Link>
          <Link href="/posts">Posts</Link>
          <Link href="/about">About</Link>
        </div>
      </nav>
      <main className="max-w-6xl mx-auto py-8">
        {children}
      </main>
    </div>
  )
}
```

Use it in your page:

```tsx
// frontend/src/pages/Posts/Index.tsx
import AppLayout from '../../layouts/AppLayout'

export default function PostsIndex({ posts }) {
  return (
    <AppLayout>
      <h1>Posts</h1>
      {/* ... */}
    </AppLayout>
  )
}
```

## Summary

| Pattern | Purpose |
|---------|---------|
| Index pages | Display lists of items |
| Show pages | Display single item details |
| Create/Edit pages | Forms for creating/editing |
| Layouts | Shared page structure |
| `<Link>` component | SPA navigation |
| `<Form>` component | Declarative form handling |
| `router` | Programmatic navigation |
