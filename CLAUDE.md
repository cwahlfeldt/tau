# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tau` is a single-binary Rust CLI that wraps a static web app (an `index.html` plus its local assets) into a desktop or mobile app by generating a minimal Tauri v2 project on the fly, building it, and copying the resulting binaries into `./build`. There is no persistent Tauri scaffold — every run regenerates one in a tempdir and (by default) deletes it.

The wrapped Tauri project's `frontendDist` points **directly at the user's source directory**. We don't copy assets, rewrite HTML, or inject any scripts — Tauri serves the source tree as-is, with correct MIME types, and the bundler embeds it into the binary. This means importmaps, `<script type="module">`, dynamic `import()`, `new URL(...)`, fetched JSON, web workers, and any other reference style the browser supports just work, because tau never tries to discover or rewrite them.

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

# Run against the bundled sample (host platform, debug profile)
cargo run -- examples/sample-app/index.html

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

# Quiet / verbose output
cargo run -- examples/sample-app/index.html --quiet

# Dev loop: spawns `cargo tauri dev` against the user's source tree.
# Reload the webview to pick up source edits — there's no automatic
# reload watcher (Tauri serves files live from frontendDist).
cargo run -- dev examples/sample-app/index.html

# Inspect / prune the shared CARGO_TARGET_DIR (see "Cache management" below)
cargo run -- cache size
cargo run -- cache prune --days 30 [--dry-run]
cargo run -- cache clear
```

The end-to-end smoke test is `cargo run -- examples/sample-app/index.html` — the resulting `.app`/`.exe`/`.AppImage`/`.apk` should appear under `./build/`.

## External dependencies the binary expects at runtime

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`) — the build step shells out to `cargo tauri build` / `cargo tauri android build` / `cargo tauri ios build`.
- `rustup` — used to query and install mobile rustup targets on demand (see [src/build.rs](src/build.rs) `ensure_targets`).
- For mobile targets, the usual Tauri prerequisites (Android SDK/NDK + `ANDROID_HOME`, Xcode for iOS).

## Architecture

Six modules orchestrated by [src/main.rs](src/main.rs):

1. **`cli`** ([src/cli.rs](src/cli.rs)) — `clap`-derived `Cli` struct. `index` is the positional arg for the default (no-subcommand) wrap path; the `cache` subcommand exposes `size` / `prune` / `clear`; the `dev` subcommand mirrors the wrap flags but spawns `cargo tauri dev`. `subcommand_negates_reqs` + `args_conflicts_with_subcommands` make the index optional only when a subcommand is given.
2. **`config`** ([src/config/mod.rs](src/config/mod.rs)) — three-tier resolution: CLI flags > `tau.conf.json` (next to the index, or cwd, or `--config`) > defaults (debug profile, host platform, output dir `./build`, identifier `com.tau.<slug>`). The resolved `Config` is passed by reference through every subsequent stage. `BuildProfile::Release(SigningConfig)` makes "release without signing" unrepresentable — that combination errors at config time.
3. **`input`** ([src/input.rs](src/input.rs)) — classifies the positional argument as either `File { source_root }` or `Url(String)`. URL inputs scaffold differently (window URL set, frontendDist points at a tiny stub dir).
4. **`scaffold`** ([src/scaffold/mod.rs](src/scaffold/mod.rs)) — writes a minimal Tauri v2 project into a tempdir: `src-tauri/{Cargo.toml, tauri.conf.json, build.rs, src/{main,lib}.rs, capabilities/default.json, icons/icon.png}`. Static templates live in [src/scaffold/templates/](src/scaffold/templates/) and are pulled in via `include_str!` / `include_bytes!`; `tauri.conf.json` is built with `serde_json::json!` and gets `frontendDist` set to an absolute path — the user's source directory for `create_for_source`, or a one-file stub dir for `create_for_url`. The icon is a real 32×32 RGBA PNG ([src/scaffold/templates/icon.png](src/scaffold/templates/icon.png)) — Tauri's bundler requires a decodable PNG, so don't replace it with a stub. `tauri.conf.json` deliberately omits `dmg`/`msi` from `bundle.targets` because the dmg bundler mounts and opens a disk image during the build.
5. **`build`** ([src/build.rs](src/build.rs)) — shells out to `cargo tauri ...` with `CARGO_TARGET_DIR` pointed at the shared cache returned by `cache::dir()`. This cache is critical: without it every run recompiles all of Tauri from scratch because the scaffold lives in a fresh tempdir. `build_desktop` / `build_mobile` split the per-platform-family logic; `extract_artifacts` handles both filtering (desktop) and renaming (mobile) artifact-collection styles described below.
6. **`cache`** ([src/cache.rs](src/cache.rs)) — owns the shared `CARGO_TARGET_DIR` path (`~/Library/Caches/tau/target` on macOS, `$XDG_CACHE_HOME/tau/target` elsewhere) and the `size` / `prune` / `clear` operations exposed by the `cache` subcommand. Pruning walks the tree and deletes files older than a cutoff, then removes empty dirs bottom-up.

Plus [src/log.rs](src/log.rs) (central `Logger` with `Quiet` / `Normal` / `Verbose` levels), [src/pipeline.rs](src/pipeline.rs) (orchestrator for the default wrap path), and [src/dev.rs](src/dev.rs) (orchestrator for `tau dev`).

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

`withGlobalTauri = true` in the generated `tauri.conf.json` makes core Tauri APIs (`invoke`, `event`, `path`, `window`, `webview`, …) available at `window.__TAURI__.<ns>` to plain `<script>`-loaded code, no bundler required. Tauri injects these as a webview init script.

Plugins (`fs`, `dialog`, `haptics`, etc.) are **not** registered by default. Users who want them should scaffold their own Tauri project — tau is for the static-site case where you want the platform shell and nothing else.

## Configuration file

Optional `tau.conf.json` next to the index file (or in the cwd, or `--config <path>`) — schema is `FileConfig` in [src/config/mod.rs](src/config/mod.rs). All fields are optional; CLI flags win over file values. `#[serde(deny_unknown_fields)]` is on, so typos in user configs become hard errors. `signing` is parsed and validated by `--release` but not yet wired into the actual `cargo tauri` invocations (release signing is a TODO — the seam is `BuildProfile::Release(SigningConfig)`).

## Testing

Each module has an in-file `#[cfg(test)] mod tests` block covering the pure functions: `Platform` parsing, identifier slugification, profile-component matching, product-name variant generation, scaffold template invariants, and frontendDist wiring. Run with `cargo test`.
