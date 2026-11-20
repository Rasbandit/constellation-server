import { readFileSync } from 'node:fs';
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
    define: {
      // Also on in the image build (CONSTELLATION_WEB_ADMIN_API=1): its /api goes to an operator proxy.
      __CONSTELLATION_ADMIN_API__: JSON.stringify(admin.passthrough !== undefined || process.env.CONSTELLATION_WEB_ADMIN_API === '1'),
      // The one reader of the web version (tools/version-check keeps it equal to Cargo.toml's).
      __WEB_VERSION__: JSON.stringify(JSON.parse(readFileSync('package.json', 'utf8')).version),
    },
    server: { port: 5173, proxy: admin.proxy },
    test: { environment: 'node' },
  };
});
