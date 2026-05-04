# tau

Wrap a static web app into a desktop or mobile app by pointing at its `index.html`.
No persistent project, no manual Tauri scaffolding — `tau` generates a minimal
Tauri v2 project on the fly, builds it, and drops the resulting binaries into
`./build/`.

## Install

```bash
cargo install --path .
```

You'll also need:

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`)
- `rustup` (mobile rustup targets are installed on demand)
- For Android: Android SDK + NDK with `ANDROID_HOME` set
- For iOS: Xcode

## Usage

### Build a bundle

```bash
# Host platform, debug profile, output in ./build/
tau examples/sample-app/index.html

# Multiple desktop targets
tau examples/sample-app/index.html -p macos,windows,linux

# Mobile
tau examples/sample-app/index.html -p android
tau examples/sample-app/index.html -p ios

# Override identity from the CLI
tau examples/sample-app/index.html --name "My App" --identifier com.example.myapp

# Inspect the generated scaffold without building
tau examples/sample-app/index.html --dry-run
```

### Hot-reload dev loop

`tau dev` runs `cargo tauri dev` against a freshly-scaffolded project and
watches your source files. Edits to HTML / CSS / JS are picked up
automatically and the webview reloads.

```bash
# Host platform
tau dev examples/sample-app/index.html

# Pick a single target (one at a time — dev attaches to one device interactively)
tau dev examples/sample-app/index.html --platform android
tau dev examples/sample-app/index.html --platform ios

# Keep the scaffold tempdir around after Ctrl-C, useful for debugging
tau dev examples/sample-app/index.html --keep-scaffold
```

### Cache management

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

Optional `tau.conf.json` in the cwd (or `--config <path>`):

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

## What's in the wrapped app

The generated webview gets Tauri's JS APIs at runtime, with no extra config:

- **Core APIs** at `window.__TAURI__.<ns>` (`invoke`, `event`, `path`,
  `window`, `webview`, …)
- **Plugins** at `window.__TAURI_PLUGIN_FS__`,
  `window.__TAURI_PLUGIN_DIALOG__`, and (mobile) `window.__TAURI_PLUGIN_HAPTICS__`

Plugin permissions for `fs` and `dialog` are granted by default on all
platforms; `haptics` is granted on iOS/Android only.

## Examples

A minimal sample app lives in [examples/sample-app/](examples/sample-app/).
Quick smoke test:

```bash
cargo run -- examples/sample-app/index.html
# => ./build/{wrappedapp.app, ...}

cargo run -- dev examples/sample-app/index.html
# => Tauri window opens; edit index.html and watch it reload
```

## Development

```bash
cargo build              # CLI
cargo test               # unit tests (~50)
cargo clippy --all-targets
```

Architecture notes and module map: [CLAUDE.md](CLAUDE.md).
