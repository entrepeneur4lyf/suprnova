import './app.css'
import { createInertiaApp, router } from '@inertiajs/vue3'
import { createApp, createSSRApp, h, type DefineComponent } from 'vue'

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
    const pages = import.meta.glob<DefineComponent>('./pages/**/*.vue', {
      eager: true,
    })
    return pages[`./pages/${name}.vue`]
  },
  setup({ el, App, props, plugin }) {
    // SSR-aware: when the server pre-rendered, the mount node carries
    // `data-server-rendered="true"` and we must hydrate. Without this
    // check, SSR markup gets destroyed and re-rendered on the client.
    if (el.hasAttribute('data-server-rendered')) {
      createSSRApp({ render: () => h(App, props) })
        .use(plugin)
        .mount(el)
    } else {
      createApp({ render: () => h(App, props) })
        .use(plugin)
        .mount(el)
    }
  },
})
