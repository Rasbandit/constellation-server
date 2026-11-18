import react from '@vitejs/plugin-react';
import { defineConfig } from 'vitest/config';
import { mockApi } from './mock/plugin';

// The mock API answers /api/* inside the dev server. When the real server has these endpoints,
// drop the plugin and proxy /api to it instead (see README.md).
export default defineConfig({
  plugins: [react(), mockApi()],
  server: { port: 5173 },
  test: { environment: 'node' },
});
