<script lang="ts">
  import { useForm } from '@inertiajs/svelte'
  import type { LoginProps } from '../../types/inertia-props'

  let { errors }: LoginProps = $props()

  const form = useForm({
    email: '',
    password: '',
    remember: false,
  })

  function submit(e: SubmitEvent) {
    e.preventDefault()
    form.post('/login')
  }
</script>

<div
  class="min-h-screen flex items-center justify-center bg-gray-50 py-12 px-4 sm:px-6 lg:px-8"
>
  <div class="max-w-md w-full space-y-8">
    <div>
      <h2 class="mt-6 text-center text-3xl font-extrabold text-gray-900">
        Sign in to your account
      </h2>
    </div>
    <form class="mt-8 space-y-6" onsubmit={submit}>
      <div class="rounded-md shadow-sm -space-y-px">
        <div>
          <label for="email" class="sr-only">Email address</label>
          <input
            id="email"
            name="email"
            type="email"
            autocomplete="email"
            required
            class="appearance-none rounded-none relative block w-full px-3 py-2 border border-gray-300 placeholder-gray-500 text-gray-900 rounded-t-md focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 focus:z-10 sm:text-sm"
            placeholder="Email address"
            bind:value={form.email}
          />
        </div>
        <div>
          <label for="password" class="sr-only">Password</label>
          <input
            id="password"
            name="password"
            type="password"
            autocomplete="current-password"
            required
            class="appearance-none rounded-none relative block w-full px-3 py-2 border border-gray-300 placeholder-gray-500 text-gray-900 rounded-b-md focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 focus:z-10 sm:text-sm"
            placeholder="Password"
            bind:value={form.password}
          />
        </div>
      </div>

      {#if errors?.email}
        <div class="text-red-600 text-sm">{errors.email[0]}</div>
      {/if}

      <div class="flex items-center">
        <input
          id="remember"
          name="remember"
          type="checkbox"
          class="h-4 w-4 text-indigo-600 focus:ring-indigo-500 border-gray-300 rounded"
          bind:checked={form.remember}
        />
        <label for="remember" class="ml-2 block text-sm text-gray-900">
          Remember me
        </label>
      </div>

      <div>
        <button
          type="submit"
          disabled={form.processing}
          class="group relative w-full flex justify-center py-2 px-4 border border-transparent text-sm font-medium rounded-md text-white bg-indigo-600 hover:bg-indigo-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-indigo-500 disabled:opacity-50"
        >
          {form.processing ? 'Signing in...' : 'Sign in'}
        </button>
      </div>

      <div class="text-center">
        <a href="/register" class="text-indigo-600 hover:text-indigo-500">
          Don't have an account? Register
        </a>
      </div>
    </form>
  </div>
</div>
