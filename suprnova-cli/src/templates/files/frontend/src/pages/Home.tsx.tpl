import type { HomeProps } from '../types/inertia-props'

export default function Home({ title, message }: HomeProps) {
  return (
    <div className="font-sans p-8 max-w-xl mx-auto">
      <h1 className="text-3xl font-bold">{title}</h1>
      <p className="mt-2">{message}</p>
      <p className="mt-8 text-gray-500">
        Edit <code className="bg-gray-100 px-1 rounded">frontend/src/pages/Home.tsx</code> to get started.
      </p>
    </div>
  )
}
