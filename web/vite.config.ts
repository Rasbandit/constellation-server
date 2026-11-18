import react from '@vitejs/plugin-react';
import { loadEnv } from 'vite';
import { defineConfig } from 'vitest/config';
import { mockApi } from './mock/plugin';
import { adminGuard, adminProxy } from './mock/proxy';

// The stub answers /api/* inside the dev server. With CONSTELLATION_ADMIN_URL set, uploads go to
// the server's admin API instead (see mock/proxy.ts and README.md).
export default defineConfig(({ mode }) => {
  const admin = adminProxy({ ...loadEnv(mode, process.cwd(), 'CONSTELLATION_'), ...process.env });
  if (mode !== 'test') console.log(`[constellation] ${admin.summary}`);
  return {
    // The guard first: it must turn away other sites before the proxy adds the token.
    plugins: [...(admin.passthrough ? [adminGuard(admin.passthrough)] : []), react(), mockApi(admin.passthrough)],
    define: { __CONSTELLATION_ADMIN_API__: JSON.stringify(admin.passthrough !== undefined) },
    server: { port: 5173, proxy: admin.proxy },
    test: { environment: 'node' },
  };
});
