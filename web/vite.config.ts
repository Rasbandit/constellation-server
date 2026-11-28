import { readFileSync } from 'node:fs';
import react from '@vitejs/plugin-react';
import { loadEnv } from 'vite';
import { defineConfig } from 'vitest/config';
import { mockApi } from './mock/plugin';
import { apiProxy } from './mock/proxy';

// The stub answers /api/* inside the dev server. With CONSTELLATION_ADMIN_URL set, /api goes to
// the server instead (see mock/proxy.ts and README.md).
export default defineConfig(({ mode }) => {
  const admin = apiProxy({ ...loadEnv(mode, process.cwd(), 'CONSTELLATION_'), ...process.env });
  if (mode !== 'test') console.log(`[constellation] ${admin.summary}`);
  return {
    plugins: [react(), mockApi(admin.passthrough)],
    define: {
      // Also on in the image build (CONSTELLATION_WEB_ADMIN_API=1): its /api is the server's own, behind the login page.
      __CONSTELLATION_ADMIN_API__: JSON.stringify(admin.passthrough !== undefined || process.env.CONSTELLATION_WEB_ADMIN_API === '1'),
      // The one reader of the web version (tools/version-check keeps it equal to Cargo.toml's).
      __WEB_VERSION__: JSON.stringify(JSON.parse(readFileSync('package.json', 'utf8')).version),
    },
    server: { port: 5173, proxy: admin.proxy },
    test: { environment: 'node' },
  };
});
