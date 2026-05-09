import { defineConfig } from 'vite';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';
import { readFileSync } from 'node:fs';

const here = dirname(fileURLToPath(import.meta.url));

// We keep package.json + node_modules inside `.tau/` so the user's project
// root stays clean (just `src/` + `.tau/`). The downside: Vite/Node's
// upward-walking resolver doesn't see `.tau/node_modules` from `src/game.js`.
// To fix that without leaking node_modules into the user's view, we read
// the dependency list from `.tau/package.json` and build explicit aliases
// pointing at `.tau/node_modules/<dep>`. New deps added via `tau add` are
// picked up automatically on next dev/build because we read package.json
// at config-load time.
function depsFromPackageJson() {
  const pkg = JSON.parse(readFileSync(resolve(here, 'package.json'), 'utf8'));
  return Object.keys({ ...pkg.dependencies, ...pkg.devDependencies });
}

const aliases = Object.fromEntries(
  depsFromPackageJson().map((name) => [name, resolve(here, 'node_modules', name)]),
);

export default defineConfig({
  root: resolve(here, '../src'),
  base: './',
  resolve: {
    alias: aliases,
    // Vite caches deps it has pre-bundled inside node_modules/.vite. Since
    // our node_modules is in `.tau/`, point its cache there too — otherwise
    // it lands in `src/node_modules/.vite` which we don't want.
    preserveSymlinks: false,
  },
  cacheDir: resolve(here, 'node_modules/.vite'),
  build: {
    outDir: resolve(here, 'dist'),
    emptyOutDir: true,
    target: 'esnext',
  },
  server: {
    port: 1420,
    strictPort: true,
    host: '127.0.0.1',
    fs: {
      // Vite restricts dev-server file serving to `root` and a few defaults.
      // Since node_modules is outside `root`, opt into serving from the
      // project root + `.tau/` so module bytes can be sent.
      allow: [resolve(here, '..'), here],
    },
  },
  clearScreen: false,
});
