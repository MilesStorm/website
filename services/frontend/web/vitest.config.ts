/// <reference types="vitest/config" />
// Component tests in jsdom, with Astro's Vite settings (the `@/` alias, React).
import { getViteConfig } from 'astro/config';

export default getViteConfig({
  test: {
    environment: 'jsdom',
    include: ['test/**/*.test.{ts,tsx}'],
    restoreMocks: true,
    unstubGlobals: true,
  },
});
