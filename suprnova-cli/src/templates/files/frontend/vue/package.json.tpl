{
  "name": "{project_name}-frontend",
  "private": true,
  "version": "0.1.0",
  "type": "module",
  "scripts": {
    "dev": "vite",
    "build": "vue-tsc --noEmit && vite build",
    "preview": "vite preview",
    "check": "vue-tsc --noEmit"
  },
  "dependencies": {
    "@inertiajs/vue3": "^3.4.0",
    "vue": "^3.5.34"
  },
  "devDependencies": {
    "@tailwindcss/forms": "^0.5.11",
    "@tailwindcss/typography": "^0.5.19",
    "@tailwindcss/vite": "^4.3.0",
    "@types/node": "^22.0.0",
    "@vitejs/plugin-vue": "^6.0.6",
    "tailwindcss": "^4.3.0",
    "typescript": "^6.0.3",
    "vite": "^8.0.10",
    "vue-tsc": "^3.2.8"
  }
}
