import './app.css'
import { createInertiaApp } from '@inertiajs/vue3'
import { createApp, createSSRApp, h, type DefineComponent } from 'vue'
import axios from 'axios'

// Configure axios defaults for CSRF protection
axios.defaults.headers.common['X-Requested-With'] = 'XMLHttpRequest'

const csrfToken = document
  .querySelector('meta[name="csrf-token"]')
  ?.getAttribute('content')
if (csrfToken) {
  axios.defaults.headers.common['X-CSRF-TOKEN'] = csrfToken
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
