// Static pages only (no adapter, no server runtime): the Rust server serves dist/ and splices
// each request's data in (src/layouts/Layout.astro). `build.format: 'file'` keeps today's URLs
// (`/profile`, served from profile.html). No <ClientRouter />: every navigation is a page load.
import { defineConfig } from 'astro/config';
import react from '@astrojs/react';
import tailwindcss from '@tailwindcss/vite';

// `astro dev` sends API calls, OAuth and the camera socket to the Rust server (ENV.md).
const bff = process.env.BFF_URL ?? 'http://localhost:8080';

export default defineConfig({
  output: 'static',
  trailingSlash: 'never',
  build: { format: 'file' },
  integrations: [react()],
  vite: {
    plugins: [tailwindcss()],
    server: {
      proxy: {
        '/api': bff,
        '/oauth': bff,
        '/ws': { target: bff, ws: true },
      },
    },
  },
});
