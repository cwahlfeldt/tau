# tau

An opinionated game-dev platform built on Tauri + Vite + React Three Fiber.
Run `tau create my-game` and you have a working spinning cube with HMR,
TypeScript, React Fast Refresh, and the core Tauri plugins (fs, dialog,
notification, haptics) already wired in.

`tau` also retains its original mode: wrap any static web app — or a remote
URL — into a desktop or mobile bundle by pointing at its `index.html`.

## Install

```bash
cargo install --path .
```

You'll also need:

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`)
- `rustup` (mobile rustup targets are installed on demand)
- Node 18+ and a JS package manager (pnpm preferred, npm fallback) — only
  for the game-project workflow
- For Android: Android SDK + NDK with `ANDROID_HOME` set
- For iOS: Xcode

## Game projects

```bash
# Scaffold a new game
tau create my-game
cd my-game

# Dev loop — Vite + Tauri webview, HMR over WebSocket
tau dev

# Add a dependency (runs inside .tau/ via the detected package manager)
tau add cannon-es

# Build for the host platform
tau build

# Mobile
tau build -p android
tau build -p ios
```

### What's in a fresh project

```
my-game/
├── src/
│   ├── index.html       # <div id="root"> + loads ./game.tsx
│   ├── game.tsx         # <Canvas> with a rotating <mesh> + HUD overlay
│   └── assets/          # drop models/textures here
├── tau.conf.json        # name + identifier (pinned in source control)
├── tsconfig.json        # editors auto-discover this from src/
├── .gitignore
└── .tau/                # hidden plumbing; tau owns this
    ├── package.json     # three, react, @react-three/fiber, drei, @tauri-apps/plugin-*, vite, typescript
    ├── vite.config.js   # @vitejs/plugin-react + alias resolution + the `tau` virtual-module plugin
    ├── tau.d.ts         # ambient types for `import ... from 'tau'`
    ├── pnpm-workspace.yaml
    └── node_modules/
```

### Imports

- `import { Canvas, useFrame, useThree, haptics, notification, dialog, fs } from 'tau'` —
  the `tau` virtual module re-exports all of `@react-three/fiber` plus the
  four Tauri plugin namespaces. One import for the scene-graph hooks and
  the platform APIs. Types live in `.tau/tau.d.ts` (auto-discovered by
  your editor via the root `tsconfig.json`).
- `import * as THREE from 'three'` — raw three.js types and utilities
  (`THREE.Vector3`, `THREE.Mesh`, materials, geometries) when r3f's
  intrinsics aren't enough.
- `import { OrbitControls, Stats, useGLTF } from '@react-three/drei'` —
  drei's helper components and hooks. Pinned in `.tau/package.json` but
  not re-exported through `tau` (drei's surface is too big and evolves
  fast).
- React itself: `import { useRef, useState, useEffect } from 'react'`.

### Plugins registered by default

- **fs** — scoped to the app data dir only (`fs:default` plus the
  appdata read/write recursive permissions). No home/desktop access.
- **dialog** — file open/save and message boxes.
- **notification** — local notifications.
- **haptics** — mobile only (Cargo target-gated + `#[cfg(mobile)]`).

If you want to widen the fs scope, change other capability settings, or
drop a plugin, edit the generated `src-tauri/capabilities/default.json`
once the tempdir is materialized — or run `--keep-scaffold` and copy the
edited config into a fork.

## Wrap a static site or URL

```bash
# Local file: host platform, debug profile, output in ./build/
tau examples/sample-app/index.html

# Remote URL
tau https://example.com --name "Example" --identifier com.example.app

# Multiple desktop targets
tau examples/sample-app/index.html -p macos,windows,linux

# Mobile
tau examples/sample-app/index.html -p android

# Override identity from the CLI
tau examples/sample-app/index.html --name "My App" --identifier com.example.myapp

# Inspect the generated scaffold without building
tau examples/sample-app/index.html --dry-run
```

Wrap output shares the same Tauri scaffold as game projects, so the four
plugins listed above are linked in there too. Binary size grows ~5MB
compared to a stripped Tauri shell.

URL wraps skip asset discovery — the wrapped webview just navigates to
the URL. No Tauri JS APIs are injected (no `window.__TAURI__`). Use a
local file if you need plugin bridges or offline asset bundling.

### Hot-reload dev loop (wrap path)

```bash
tau dev examples/sample-app/index.html
```

Edits to HTML / CSS / JS are picked up automatically. No Vite — Tauri
serves files directly from your source dir.

## Cache management

All tau builds share a single `CARGO_TARGET_DIR` so Tauri's deps don't
recompile from scratch on every run.

```bash
tau cache size                     # show path + size on disk
tau cache prune --days 30          # delete entries older than N days
tau cache prune --days 30 --dry-run
tau cache clear                    # nuke the whole thing
```

The cache lives at:

- macOS: `~/Library/Caches/tau/target`
- Linux: `$XDG_CACHE_HOME/tau/target`

## Configuration

Optional `tau.conf.json` next to your `index.html` (or in the cwd, or
`--config <path>`):

```json
{
  "name": "My App",
  "version": "0.1.0",
  "identifier": "com.example.myapp",
  "include": ["assets/**", "fonts/*.woff2"],
  "build": {
    "output": "./dist",
    "platforms": ["macos", "android"]
  }
}
```

CLI flags always win over file values. Unknown fields are rejected.

## Development

```bash
cargo build              # CLI
cargo test               # unit tests
cargo clippy --all-targets
```

Architecture notes and module map: [CLAUDE.md](CLAUDE.md).
