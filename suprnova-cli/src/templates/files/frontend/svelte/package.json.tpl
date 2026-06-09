{
  "name": "{project_name}-frontend",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "svelte-check --tsconfig ./tsconfig.json && vite build",
    "preview": "vite preview",
    "check": "svelte-check --tsconfig ./tsconfig.json"
  },
  "dependencies": {
    "@inertiajs/svelte": "^3.1.1",
    "svelte": "^5.55.5"
  },
  "devDependencies": {
    "@sveltejs/vite-plugin-svelte": "^7.1.2",
    "@tailwindcss/forms": "^0.5.11",
    "@tailwindcss/typography": "^0.5.19",
    "@tailwindcss/vite": "^4.3.0",
    "@tsconfig/svelte": "^5.0.8",
    "@types/node": "^22.0.0",
    "svelte-check": "^4.4.8",
    "tailwindcss": "^4.3.0",
    "typescript": "^6.0.3",
    "vite": "^8.0.10"
  }
}
