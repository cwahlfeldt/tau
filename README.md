# tau

A lightweight, single-binary CLI that wraps any web project — a single
`index.html`, a pre-built React/Vite `dist/`, a Svelte build, or a remote
URL — into a desktop or mobile app. Powered by Tauri v2, with no
configuration required until you want it.

## Install

```bash
cargo install --path .
```

You'll also need:

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`)
- `rustup` (mobile rustup targets are installed on demand)
- For Android: Android SDK + NDK with `ANDROID_HOME` set
- For iOS: Xcode

## Quickstart

```bash
# Point at a directory containing index.html (e.g. a Vite/React/Svelte build)
tau ./dist

# Or a single index.html
tau ./examples/sample-app/index.html

# Or a remote URL
tau https://example.com

# Pin a name + bundle identifier without touching the CLI flags every time
tau init
# (creates tau.conf.json in the current directory)

# Iterate with a dev loop — Tauri webview pointed at your source tree
tau dev ./dist

# Explicit build subcommand (identical to the top-level form)
tau build ./dist -p macos,windows,linux

# Mobile
tau build ./dist -p android
tau build ./dist -p ios

# Inspect the generated Tauri scaffold without building
tau ./dist --dry-run
```

Output binaries land in `./build/` by default.

## Tauri APIs available out of the box

Every wrapped app gets `withGlobalTauri: true` and a default set of
plugins pre-registered. Your code can call them directly via
`window.__TAURI__.<namespace>` from plain `<script>` tags — no bundler,
no `@tauri-apps/*` imports needed.

| Plugin | Where | Capability |
| --- | --- | --- |
| **fs** | desktop + mobile | `fs:default` plus appdata read/write recursive. Scoped to the app data dir only — no home or desktop access. |
| **dialog** | desktop + mobile | File open/save and message boxes. |
| **notification** | desktop + mobile | Local notifications. |
| **haptics** | mobile only | Cargo target-gated + `#[cfg(mobile)]`. Vibrate, impact/notification/selection feedback. |

Plus all Tauri core APIs (`invoke`, `event`, `path`, `window`, `webview`, …).
This adds ~5MB to the bundle compared to a stripped Tauri shell.

URL wraps point the webview at the remote URL and skip Tauri's JS API
injection (no `window.__TAURI__`). Use a local file or directory if you
need the plugin bridges.

## Configuration

Everything works without it. When you do want to pin things, run
`tau init` to drop a starter `tau.conf.json` in the current directory,
or hand-write one next to your `index.html`:

```json
{
  "name": "My App",
  "version": "0.1.0",
  "identifier": "com.example.myapp",
  "exclude": ["secrets/**"],
  "permissions": [
    "fs:allow-audio-write-recursive",
    "fs:allow-document-read-recursive"
  ],
  "build": {
    "output": "./dist",
    "platforms": ["macos", "android"]
  }
}
```

Resolution order: CLI flags > `tau.conf.json` > defaults. Unknown fields
are rejected.

`permissions` appends Tauri capability identifiers to the default
cross-platform capability — useful when you need fs access outside the
app data dir, or want to grant additional plugin permissions without
forking the scaffold. The defaults (`core:default`, `fs:default`,
`fs:allow-appdata-{read,write}-recursive`, `dialog:default`,
`notification:default`) are always included.

`tau init` flags:

```bash
tau init --name "My App" --identifier com.example.myapp
tau init --force                # overwrite an existing tau.conf.json
```

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

## Examples

The `examples/` directory contains a few inputs to try:

- `examples/sample-app/` — hello world: HTML, CSS, an image, a script.
- `examples/configured-app/` — same shape, plus a `tau.conf.json`.
- `examples/haptics-demo/` — exercises the mobile haptics plugin.
- `examples/r3f-demo/` — React Three Fiber app with notification, dialog,
  and fs plugin demos. Source in `src/`, committed Vite build in `dist/`.
- `examples/blade-dash/` — a non-trivial pre-built site you can wrap.

## Development

```bash
cargo build
cargo test
cargo clippy --all-targets
```

Architecture notes and module map: [CLAUDE.md](CLAUDE.md).
