import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwind from '@tailwindcss/vite';
import path from 'node:path';

// `npm run build` writes into engine/ui, which `cargo build` embeds in the powerdb binary.
export default defineConfig({
  plugins: [react(), tailwind()],
  resolve: { alias: { '@sdk': path.resolve(__dirname, '../sdk/src/index.ts') } },
  build: { outDir: '../engine/ui', emptyOutDir: true, sourcemap: false, chunkSizeWarningLimit: 700 },
  server: { proxy: { '/api': 'http://127.0.0.1:8090' } },
});
