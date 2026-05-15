# r3f-demo

A React Three Fiber app wrapped by `tau`. Three orbiting cubes; clicking
each one fires a different Tauri plugin (notification, dialog, fs).

## Layout

- `src/` — React + R3F source (Vite project root).
- `dist/` — committed Vite production build. This is what `tau` wraps.
- `tau.conf.json` — pins the app name + bundle identifier.
- `package.json`, `vite.config.js` — the bundler config.

The build output is checked in so `tau` works without a Node toolchain. If
you edit `src/`, rebuild with the steps below before re-running `tau`.

## Rebuild the frontend

```bash
cd examples/r3f-demo
npm install
npm run build
```

This writes a fresh `dist/`.

## Wrap with tau

Run from the example directory so `tau.conf.json` (next to this README,
not inside `dist/`) is picked up — that's where the app name and bundle
identifier live.

```bash
cd examples/r3f-demo

# Build a desktop bundle
tau dist/

# Or the explicit subcommand
tau build dist/

# Dev loop — webview pointed at the built dist. Re-run `npm run build`
# and reload the webview to see changes (no HMR; tau wraps pre-built sites).
tau dev dist/

# Mobile
tau build dist/ -p android
```

Output lands in `./build/` at the repo root. If you'd rather run from the
repo root, pass `--config` explicitly:

```bash
cargo run -- examples/r3f-demo/dist/ --config examples/r3f-demo/tau.conf.json
```

## What the plugins demonstrate

- **notification cube (blue)** — `window.__TAURI_PLUGIN_NOTIFICATION__`. Requests permission on first click, then sends a system notification.
- **dialog cube (orange)** — `window.__TAURI_PLUGIN_DIALOG__`. Opens a native file picker; prints the selected path in the status line.
- **fs cube (green)** — `window.__TAURI_PLUGIN_FS__`. Writes a counter to `<appdata>/r3f-demo-counter.txt` (scoped by tau's default `fs` capability to the app data dir only) and reads it back on launch.

All three are wired in by tau's default capability set — nothing to
configure in `tau.conf.json`.
