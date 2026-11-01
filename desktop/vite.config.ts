import { defineConfig } from 'vite';
import react from '@vitejs/plugin-react';
import tailwind from '@tailwindcss/vite';
import path from 'node:path';

// The Manager shares the dashboard's components and theme tokens (@dash), so both look like one product.
export default defineConfig({
  plugins: [react(), tailwind()],
  resolve: {
    alias: { '@dash': path.resolve(__dirname, '../dashboard/src') },
    // The shared dashboard files live outside this package. Without this they would find their own copy of React in
    // dashboard/node_modules (or none at all in CI), and two copies of React break hooks.
    dedupe: ['react', 'react-dom'],
  },
  clearScreen: false,
  server: { port: 1420, strictPort: true, fs: { allow: ['..'] } },
  build: { outDir: 'dist', emptyOutDir: true, sourcemap: false, target: 'es2022' },
});
