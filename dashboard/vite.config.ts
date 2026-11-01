import { defineConfig } from 'vitest/config';
import react from '@vitejs/plugin-react';
import tailwind from '@tailwindcss/vite';
import path from 'node:path';

// `npm run build` writes into engine/ui, which `cargo build` embeds in the powerdb binary.
export default defineConfig({
  plugins: [react(), tailwind()],
  resolve: { alias: { '@sdk': path.resolve(__dirname, '../sdk/src/index.ts') } },
  build: { outDir: '../engine/ui', emptyOutDir: true, sourcemap: false, chunkSizeWarningLimit: 700 },
  server: { proxy: { '/api': 'http://127.0.0.1:8090' } },
  test: {
    // Real server, real API: globalSetup starts `powerdb serve --ephemeral`, and the page "lives" at its address.
    environment: 'jsdom',
    environmentOptions: { jsdom: { url: `http://127.0.0.1:${process.env.DASH_TEST_PORT ?? 18090}/` } },
    globalSetup: './test/server.ts',
    setupFiles: './test/setup.ts',
    include: ['test/**/*.test.tsx'],
    testTimeout: 30000,
    fileParallelism: false,
  },
});
