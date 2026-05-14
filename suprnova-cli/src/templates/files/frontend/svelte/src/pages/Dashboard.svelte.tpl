<script lang="ts">
  import { router } from '@inertiajs/svelte'
  import type { DashboardProps } from '../types/inertia-props'

  let { user }: DashboardProps = $props()

  function handleLogout() {
    router.post('/logout')
  }
</script>

<div class="min-h-screen bg-gray-100">
  <nav class="bg-white shadow">
    <div class="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
      <div class="flex justify-between h-16">
        <div class="flex items-center">
          <span class="text-xl font-semibold">Dashboard</span>
        </div>
        <div class="flex items-center space-x-4">
          <span class="text-gray-700">{user.name}</span>
          <button
            onclick={handleLogout}
            class="text-gray-500 hover:text-gray-700 px-3 py-2 rounded-md text-sm font-medium"
          >
            Logout
          </button>
        </div>
      </div>
    </div>
  </nav>

  <main class="max-w-7xl mx-auto py-6 sm:px-6 lg:px-8">
    <div class="px-4 py-6 sm:px-0">
      <div
        class="border-4 border-dashed border-gray-200 rounded-lg h-96 flex items-center justify-center"
      >
        <div class="text-center">
          <h2 class="text-2xl font-bold text-gray-900">
            Welcome, {user.name}!
          </h2>
          <p class="mt-2 text-gray-600">You are logged in.</p>
          <p class="mt-4 text-sm text-gray-500">Email: {user.email}</p>
        </div>
      </div>
    </div>
  </main>
</div>
