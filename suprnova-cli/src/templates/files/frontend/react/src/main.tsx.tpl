import './app.css'
import { createInertiaApp, router, type ResolvedComponent } from '@inertiajs/react'
import { createRoot, hydrateRoot } from 'react-dom/client'

// Forward the per-session CSRF token (rendered into <meta name="csrf-token">
// by the Suprnova CSRF middleware) on every Inertia visit. Inertia 3 uses
// the native fetch API and sets X-Inertia automatically, so no axios.
const csrfToken = document
  .querySelector('meta[name="csrf-token"]')
  ?.getAttribute('content')
if (csrfToken) {
  router.on('before', (event) => {
    event.detail.visit.headers['X-CSRF-TOKEN'] = csrfToken
  })
}

createInertiaApp({
  resolve: (name) => {
    const pages = import.meta.glob<ResolvedComponent>('./pages/**/*.tsx', {
      eager: true,
      import: 'default',
    })
    return pages[`./pages/${name}.tsx`]
  },
  setup({ el, App, props }) {
    if (el.hasAttribute('data-server-rendered')) {
      hydrateRoot(el, <App {...props} />)
    } else {
      createRoot(el).render(<App {...props} />)
    }
  },
})
