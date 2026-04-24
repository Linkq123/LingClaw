import { defineConfig } from 'vite';

export default defineConfig({
  root: '.',
  publicDir: 'public',
  build: {
    outDir: '../static',
    emptyOutDir: true,
    sourcemap: false,
    modulePreload: {
      resolveDependencies(_filename, deps) {
        return deps.filter((dep) => !/\b(hljs|marked|katex)-/.test(dep));
      },
    },
    rollupOptions: {
      output: {
        manualChunks(id) {
          const normalizedId = id.replace(/\\/g, '/');
          if (normalizedId.includes('commonjsHelpers.js')) return 'vendor-utils';
          if (normalizedId.includes('/node_modules/react')) return 'react';
          if (normalizedId.includes('/node_modules/react-dom')) return 'react';
          if (normalizedId.includes('/node_modules/highlight.js/styles/')) return undefined;
          if (normalizedId.includes('/node_modules/highlight.js')) return 'hljs';
          if (normalizedId.includes('/node_modules/katex')) return 'katex';
          if (normalizedId.includes('/node_modules/marked')) return 'marked';
          if (normalizedId.includes('/node_modules/marked-highlight')) return 'marked';
          if (normalizedId.includes('/node_modules/dompurify')) return 'marked';
        },
      },
    },
  },
  server: {
    port: 5173,
    proxy: {
      '/ws': {
        target: 'http://localhost:18989',
        ws: true,
      },
      '/api': {
        target: 'http://localhost:18989',
      },
    },
  },
  test: {
    environment: 'jsdom',
    include: ['tests/**/*.test.ts'],
  },
});
