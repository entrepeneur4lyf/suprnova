<script lang="ts">
  import { useForm } from '@inertiajs/svelte'
  import type { RegisterProps } from '../../types/inertia-props'

  let { errors }: RegisterProps = $props()

  const form = useForm({
    name: '',
    email: '',
    password: '',
    password_confirmation: '',
  })

  function submit(e: SubmitEvent) {
    e.preventDefault()
    form.post('/register')
  }
</script>

<div
  class="min-h-screen flex items-center justify-center bg-gray-50 py-12 px-4 sm:px-6 lg:px-8"
>
  <div class="max-w-md w-full space-y-8">
    <div>
      <h2 class="mt-6 text-center text-3xl font-extrabold text-gray-900">
        Create your account
      </h2>
    </div>
    <form class="mt-8 space-y-6" onsubmit={submit}>
      <div class="space-y-4">
        <div>
          <label for="name" class="block text-sm font-medium text-gray-700">Name</label>
          <input
            id="name"
            name="name"
            type="text"
            required
            class="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm"
            bind:value={form.name}
          />
          {#if errors?.name}
            <p class="mt-1 text-sm text-red-600">{errors.name[0]}</p>
          {/if}
        </div>

        <div>
          <label for="email" class="block text-sm font-medium text-gray-700">Email address</label>
          <input
            id="email"
            name="email"
            type="email"
            autocomplete="email"
            required
            class="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm"
            bind:value={form.email}
          />
          {#if errors?.email}
            <p class="mt-1 text-sm text-red-600">{errors.email[0]}</p>
          {/if}
        </div>

        <div>
          <label for="password" class="block text-sm font-medium text-gray-700">Password</label>
          <input
            id="password"
            name="password"
            type="password"
            required
            class="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm"
            bind:value={form.password}
          />
          {#if errors?.password}
            <p class="mt-1 text-sm text-red-600">{errors.password[0]}</p>
          {/if}
        </div>

        <div>
          <label for="password_confirmation" class="block text-sm font-medium text-gray-700"
            >Confirm Password</label
          >
          <input
            id="password_confirmation"
            name="password_confirmation"
            type="password"
            required
            class="mt-1 block w-full px-3 py-2 border border-gray-300 rounded-md shadow-sm focus:outline-none focus:ring-indigo-500 focus:border-indigo-500 sm:text-sm"
            bind:value={form.password_confirmation}
          />
          {#if errors?.password_confirmation}
            <p class="mt-1 text-sm text-red-600">{errors.password_confirmation[0]}</p>
          {/if}
        </div>
      </div>

      <div>
        <button
          type="submit"
          disabled={form.processing}
          class="w-full flex justify-center py-2 px-4 border border-transparent rounded-md shadow-sm text-sm font-medium text-white bg-indigo-600 hover:bg-indigo-700 focus:outline-none focus:ring-2 focus:ring-offset-2 focus:ring-indigo-500 disabled:opacity-50"
        >
          {form.processing ? 'Creating account...' : 'Register'}
        </button>
      </div>

      <div class="text-center">
        <a href="/login" class="text-indigo-600 hover:text-indigo-500">
          Already have an account? Sign in
        </a>
      </div>
    </form>
  </div>
</div>
