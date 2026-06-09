import './app.css'
import { createInertiaApp, router, type ResolvedComponent } from '@inertiajs/svelte'
import { hydrate, mount } from 'svelte'

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
    const pages = import.meta.glob<ResolvedComponent>('./pages/**/*.svelte', {
      eager: true,
    })
    return pages[`./pages/${name}.svelte`]
  },
  setup({ el, App, props }) {
    if (el?.hasAttribute('data-server-rendered')) {
      hydrate(App, { target: el, props })
    } else {
      mount(App, { target: el!, props })
    }
  },
})
