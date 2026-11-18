import react from '@vitejs/plugin-react';
import { defineConfig } from 'vite';

// One self-contained bundle for the Claude artifact (`bun run build:artifact`).
export default defineConfig({
  plugins: [react()],
  build: {
    outDir: 'dist-artifact',
    emptyOutDir: true,
    cssCodeSplit: false,
    modulePreload: false,
    rollupOptions: {
      input: 'src/main-artifact.tsx',
      output: { entryFileNames: 'app.js', assetFileNames: 'app[extname]', inlineDynamicImports: true },
    },
  },
});
