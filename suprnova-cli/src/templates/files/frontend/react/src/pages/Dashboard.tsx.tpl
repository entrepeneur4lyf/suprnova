import { router } from '@inertiajs/react'
import type { DashboardProps } from '../types/inertia-props'

export default function Dashboard({ user }: DashboardProps) {
  const handleLogout = () => {
    router.post('/logout')
  }

  return (
    <div className="min-h-screen bg-gray-100">
      <nav className="bg-white shadow">
        <div className="max-w-7xl mx-auto px-4 sm:px-6 lg:px-8">
          <div className="flex justify-between h-16">
            <div className="flex items-center">
              <span className="text-xl font-semibold">Dashboard</span>
            </div>
            <div className="flex items-center space-x-4">
              <span className="text-gray-700">{user.name}</span>
              <button
                onClick={handleLogout}
                className="text-gray-500 hover:text-gray-700 px-3 py-2 rounded-md text-sm font-medium"
              >
                Logout
              </button>
            </div>
          </div>
        </div>
      </nav>

      <main className="max-w-7xl mx-auto py-6 sm:px-6 lg:px-8">
        <div className="px-4 py-6 sm:px-0">
          <div className="border-4 border-dashed border-gray-200 rounded-lg h-96 flex items-center justify-center">
            <div className="text-center">
              <h2 className="text-2xl font-bold text-gray-900">
                Welcome, {user.name}!
              </h2>
              <p className="mt-2 text-gray-600">
                You are logged in.
              </p>
              <p className="mt-4 text-sm text-gray-500">
                Email: {user.email}
              </p>
            </div>
          </div>
        </div>
      </main>
    </div>
  )
}
