# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tau` is a single-binary Rust CLI that wraps a static web app (an `index.html` plus its local assets) or a remote URL into a desktop or mobile app. It generates a minimal Tauri v2 project on the fly, builds it, and copies the resulting binaries into `./build`. There is no persistent Tauri scaffold — every run regenerates one in a tempdir and (by default) deletes it.

Input shapes that "just work":

- A directory containing `index.html` (e.g. a Vite/React/Svelte `dist/`) — `tau ./dist`.
- A single `index.html` file — `tau ./examples/sample-app/index.html`.
- A remote URL — `tau https://example.com`.

The wrapped Tauri project's `frontendDist` points at a filtered copy of the user's source tree (see `filter.rs`); we don't rewrite HTML or inject anything.

## Common commands

```bash
# Build / check the CLI itself
cargo build
cargo build --release
cargo check

# Run all unit tests (in-module #[cfg(test)] blocks)
cargo test

# Lint — currently clippy-clean
cargo clippy --all-targets

# --- Wrap a local site --------------------------------------------------

# Run against the bundled sample (host platform, debug profile)
cargo run -- examples/sample-app/index.html
cargo run -- examples/sample-app/        # directory shortcut

# Generate the Tauri scaffold without building — useful for inspecting the
# generated src-tauri/ tree. The temp dir is leaked and printed.
cargo run -- examples/sample-app/index.html --dry-run

# Keep the scaffold after a successful build (for debugging codegen)
cargo run -- examples/sample-app/index.html --keep-scaffold

# Target specific platforms
cargo run -- examples/sample-app/index.html -p macos,windows,linux
cargo run -- examples/sample-app/index.html -p android
cargo run -- examples/sample-app/index.html -p ios

# Override identity from the CLI (otherwise read from tau.conf.json)
cargo run -- examples/sample-app/index.html --name "My App" --identifier com.example.myapp

# Wrap a remote URL (devUrl-less, just a window pointed at the URL)
cargo run -- https://example.com

# Explicit build subcommand — identical to the top-level form
cargo run -- build examples/sample-app/index.html -p macos

# Dev loop: spawns `cargo tauri dev` against the user's source tree.
# Reload the webview to pick up source edits — there's no watcher.
cargo run -- dev examples/sample-app/

# Pin name + identifier in the current directory
cargo run -- init --name "Demo" --identifier com.demo.app

# --- Cache ---------------------------------------------------------------

cargo run -- cache size
cargo run -- cache prune --days 30 [--dry-run]
cargo run -- cache clear
```

End-to-end smoke test: `cargo run -- examples/sample-app/index.html` should produce `./build/<bundle>`.

## External dependencies the binary expects at runtime

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`) — every build shells out to `cargo tauri ...`.
- `rustup` — used to query and install mobile rustup targets on demand (see [src/build.rs](src/build.rs) `ensure_targets`).
- For mobile targets, the usual Tauri prerequisites (Android SDK/NDK + `ANDROID_HOME`, Xcode for iOS).

## Architecture

Each module has a single concern. The dispatch in [src/main.rs](src/main.rs) routes to one of:

- **No subcommand** (top-level wrap) → [src/pipeline.rs](src/pipeline.rs) `run`
- **`build`** → [src/pipeline.rs](src/pipeline.rs) `run_build` (same body, different entry args)
- **`dev`** → [src/dev.rs](src/dev.rs)
- **`init`** → [src/init.rs](src/init.rs)
- **`cache`** → [src/cache.rs](src/cache.rs)

### The modules

1. **`cli`** ([src/cli.rs](src/cli.rs)) — `clap`-derived `Cli` plus two shared flag groups: `CommonFlags` (`-q`/`-v`, `global = true` so they appear on every subcommand) and `BuildFlags` (`--release`, `--name`, `--identifier`, `--output`, `--config`, `--keep-scaffold`). The top-level wrap form uses both flat on `Cli`; the `Build` subcommand reuses `BuildFlags` via `#[command(flatten)]`. `subcommand_negates_reqs` + `args_conflicts_with_subcommands` make the positional `index` optional when a subcommand is given.

2. **`config`** ([src/config/mod.rs](src/config/mod.rs)) — `Config` is the immutable, resolved handle every downstream stage consumes. `resolve(cwd, index_dir, &Overrides)` does three-tier layering: caller `Overrides` > `tau.conf.json` > defaults. `Overrides` is a small explicit struct (not the `Cli`); the wrap, dev, and build paths each build one from their own args.

3. **`input`** ([src/input.rs](src/input.rs)) — `Input::parse` classifies a positional argument as a local file (`File { source_root }`) or a remote URL. If the path resolves to a directory, the parser looks for `index.html` inside it and uses that as the index file; otherwise the path is taken verbatim. `source_root` is always the parent of the resolved `index.html`.

4. **`scaffold`** ([src/scaffold/mod.rs](src/scaffold/mod.rs)) — writes a minimal Tauri v2 project into a tempdir: `src-tauri/{Cargo.toml, tauri.conf.json, build.rs, src/{main,lib}.rs, capabilities/{default,mobile}.json, icons/icon.png}`. Templates live in [src/scaffold/templates/](src/scaffold/templates/). Two entry points, one per frontend source:
   - `create_for_source` — `frontendDist` = a materialized filtered copy of the user's source dir.
   - `create_for_url` — the webview's window URL is remote; `frontendDist` is a stub dir Tauri's bundler insists on.

5. **`build`** ([src/build.rs](src/build.rs)) — the low-level driver. `TauriCmd` builds `cargo tauri ...` invocations with `CARGO_TARGET_DIR` pointed at `cache::dir()`. `build_platform` runs the build per platform; `extract_artifacts` filters/renames bundles into the output dir. `spawn_dev_desktop` / `spawn_dev_mobile` return a `Child` for the dev path to manage.

6. **`pipeline`** ([src/pipeline.rs](src/pipeline.rs)) — orchestrator for both the top-level wrap and the `tau build <index>` subcommand. Both go through the same body: parse `Input`, resolve `Config`, materialize source tree via `filter`, scaffold, build, extract.

7. **`dev`** ([src/dev.rs](src/dev.rs)) — orchestrator for `tau dev <index>`. Scaffolds a temp Tauri project pointing `frontendDist` at the input source tree (or a URL stub) and spawns `cargo tauri dev`. Installs a SIGINT handler and uses process-group signaling so Ctrl+C tears down the whole subprocess tree.

8. **`init`** ([src/init.rs](src/init.rs)) — writes a starter `tau.conf.json` into the cwd with `{ name, version, identifier }`. Defaults: name from the cwd directory name, identifier slugified via `default_identifier`. `--force` overwrites an existing file. Nothing else is written — no index.html, no `.gitignore`.

9. **`filter`** ([src/filter.rs](src/filter.rs)) — materialize a filtered copy of the user's source tree so `.git/`, `node_modules/`, prior `build/` output, etc. don't end up embedded in the bundle.

10. **`cache`** ([src/cache.rs](src/cache.rs)) — owns the shared `CARGO_TARGET_DIR` path (`~/Library/Caches/tau/target` on macOS, `$XDG_CACHE_HOME/tau/target` elsewhere) and the `size`/`prune`/`clear` operations exposed by the `cache` subcommand.

11. **`log`** ([src/log.rs](src/log.rs)) — tiny structured logger (`Quiet` / `Normal` / `Verbose`).

12. **`signing`** ([src/signing.rs](src/signing.rs)) — parses optional Android keystore signing material from `tau.conf.json` and patches the generated Gradle build to wire it up. Apple signing is parsed for forward-compat but not yet wired.

### Why we don't discover or rewrite assets

An earlier design parsed `index.html`, hunted for asset references (`script[src]`, `link[href]`, `img[src]`, `srcset`, CSS `url(...)`, importmaps, etc.), rewrote absolute paths to relative, and copied only the referenced subset into a `dist/` we owned. It was fragile: anything we missed (dynamic `import()`, `new URL(...)`, fetched JSON, web workers, importmap dir prefixes that imported files we didn't walk) became a 404 → "MIME 'text/html' is not a valid JavaScript MIME type" error in the browser.

The current design lets Tauri serve the user's source directory directly via `frontendDist`. Tauri already does cross-platform packaging well — we lean into that and stop fighting it. The tradeoff: users can't use `<script src="/foo.js">`-style absolute paths anymore (they need `./foo.js`), but everything else just works.

### Why the wrapped crate has a fixed name and disables incremental

The generated `src-tauri/Cargo.toml` ([src/scaffold/templates/Cargo.toml.tmpl](src/scaffold/templates/Cargo.toml.tmpl)) hard-codes `name = "tau_app"` and sets `[profile.dev] incremental = false`. Both choices exist to keep the shared `CARGO_TARGET_DIR` from blowing up:

- **Fixed crate name**: an earlier version slugified `cfg.name` into the crate name, which meant wrapping `Foo` and `Bar` produced two unrelated crate graphs. Cargo fingerprints transitive deps by `(top-level crate, features, …)`, so the same versions of `tauri`/`url`/`toml`/etc. would land in `target/debug/deps/` once per wrapped app, sometimes 30–60+ copies of common crates. Pinning the top-level name lets every wrap reuse the same compiled artifacts. Per-app branding lives in `tauri.conf.json` (`productName` + `identifier`), which is where Tauri actually reads it for bundling — the Rust crate name is invisible to end users.
- **Incremental disabled**: every wrap runs in a fresh tempdir, so the per-package `target/debug/incremental/<crate>-<hash>/` state from a previous run is never reused (the hash is keyed on the project path). Leaving incremental on just writes 100+ MB of dead state per wrap.

If you ever change the fixed crate name, expect to invalidate every user's cache. The `cargo_template_uses_fixed_crate_name` test in [src/scaffold/mod.rs](src/scaffold/mod.rs) guards against accidental drift.

### Single-source-of-truth: `PLATFORM_SPECS` table

In [src/config/platform.rs](src/config/platform.rs), `PLATFORM_SPECS: &[PlatformSpec]` holds _everything_ per-platform: canonical name, parse aliases, `rustup` targets, artifact extensions, and whether the artifact extractor should filter by name. Adding a new platform is a one-data-entry change. `Platform::spec()` is the only lookup point.

### Two artifact-extraction quirks worth knowing

Both live in [src/build.rs](src/build.rs):

- **Desktop name filtering** (`ArtifactPolicy::FilterByProductName`): desktop bundles land in the _shared_ cache `target/`, which accumulates artifacts from previous runs and from other apps wrapped earlier. Desktop extraction therefore filters by filenames starting with the current product name (lowercased, with space→`-` and space→`_` variants). If you rename product output and lose artifacts, this filter is the first place to look.
- **Mobile name rewriting** (`ArtifactPolicy::RenameBySlug`): Android/iOS bundles use generic Gradle/Xcode names like `app-universal-debug.apk` that would collide across apps in a shared output dir. Mobile artifacts are renamed to `<slug>.<ext>` on copy.

The shared `copy_matching` helper takes a `dest_for` closure to express the per-family naming policy.

### Tauri JS APIs in wrapped apps

This is the headline feature: wrapped apps get seamless access to Tauri's core APIs and a default plugin set with zero configuration.

`withGlobalTauri = true` in the generated `tauri.conf.json` makes both core Tauri APIs (`invoke`, `event`, `path`, `window`, `webview`, …) and the registered plugins available at `window.__TAURI__.<ns>` to plain `<script>`-loaded code, no bundler required. Tauri injects these as a webview init script.

**Plugins registered by default** (via the shared `lib.rs.tmpl`):

- `tauri-plugin-fs`, `tauri-plugin-dialog`, `tauri-plugin-notification` on all platforms.
- `tauri-plugin-haptics` on mobile only — the Cargo dep is `[target.cfg(any(target_os = "android", target_os = "ios"))]` and the Rust `.plugin(...)` call is `#[cfg(mobile)]`-gated.

Capabilities are split across two files written by `write_src_tauri` ([src/scaffold/mod.rs](src/scaffold/mod.rs)):

- `capabilities/default.json` (`default_capability(extra)`): cross-platform — `core:default`, `fs:default`, `fs:allow-appdata-read-recursive`, `fs:allow-appdata-write-recursive`, `dialog:default`, `notification:default`. fs is scoped to the app data dir only by default; users widen it via `tau.conf.json` → `permissions: [...]`, which is appended to this list (dedup'd) before writing the capability file.
- `capabilities/mobile.json` (`mobile_capability()`): `platforms: ["android", "iOS"]` and the four `haptics:allow-*` permissions.

The split is **required**, not aesthetic. Listing `haptics:*` in the cross-platform capability fails desktop builds with `Permission haptics:allow-vibrate not found` — Tauri's permission validator rejects unknown identifiers as a hard error, not a warning, and `tauri-plugin-haptics` is target-gated to mobile in `Cargo.toml.tmpl`. The `platforms` field on the mobile capability is the only mechanism to scope permissions per-target.

The bundle ships ~5MB larger than a stripped Tauri shell as a result of the always-on plugins. If a plugin-free variant is ever wanted, split `lib.rs.tmpl` and branch in `write_src_tauri`.

## Configuration file

Optional `tau.conf.json` next to the index file (or in the cwd, or `--config <path>`) — schema is `FileConfig` in [src/config/mod.rs](src/config/mod.rs). All fields are optional; CLI flags win over file values. `#[serde(deny_unknown_fields)]` is on, so typos in user configs become hard errors. `signing` is parsed and validated by `--release` but not yet wired into the actual `cargo tauri` invocations (release signing is a TODO — the seam is `BuildProfile::Release` + `SigningConfig`).

`tau init` writes a minimal starter config (`{ name, version, identifier }`) into the current directory. Run it whenever you want to pin those values in source control instead of passing them as CLI flags.

## Testing

Each module has an in-file `#[cfg(test)] mod tests` block covering the pure functions: `Platform` parsing, identifier slugification, profile-component matching, product-name variant generation, scaffold template invariants, frontendDist wiring, `Input::parse` (URLs, files, directories with/without `index.html`), `init` (writes/refuses-overwrite/force). Run with `cargo test`.
