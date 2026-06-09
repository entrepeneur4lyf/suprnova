import { defineConfig } from 'vite'
import vue from '@vitejs/plugin-vue'
import tailwindcss from '@tailwindcss/vite'

export default defineConfig({
  plugins: [tailwindcss(), vue()],
  server: {
    // `suprnova serve` sets VITE_PORT to the port it resolved (the
    // distinctive 5765 default, or a scanned free port). Falling back to
    // 5765 keeps a bare `npm run dev` off the squatted 5173.
    port: Number(process.env.VITE_PORT) || 5765,
    strictPort: true,
    cors: true,
  },
  build: {
    outDir: '../public/assets',
    manifest: true,
    rollupOptions: {
      input: 'src/main.ts',
    },
  },
})
