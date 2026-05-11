# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## What this is

`tau` is a single-binary Rust CLI that wraps a static web app (an `index.html` plus its local assets) into a desktop or mobile app by generating a minimal Tauri v2 project on the fly, building it, and copying the resulting binaries into `./build`. There is no persistent Tauri scaffold — every run regenerates one in a tempdir and (by default) deletes it.

`tau` has two surfaces:

1. **Wrap an arbitrary static site or URL** — `tau path/to/index.html` or `tau https://example.com`. The wrapped Tauri project's `frontendDist` points at the user's source directory (via a filtered copy — see `filter.rs`); we don't rewrite HTML or inject anything.
2. **Game-engine workflow** — `tau create my-game` scaffolds a Vite + three.js project with hidden tooling in `.tau/`. `tau dev` runs Vite + a Tauri webview pointed at it (HMR works). `tau build` runs `vite build` then `cargo tauri build`. `tau add <pkg>` shells out to pnpm/npm inside `.tau/`.

Both paths share the same Tauri scaffold/build code.

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

# --- Wrap-anything path -------------------------------------------------

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

# Wrap a remote URL (devUrl-less, just a window pointed at the URL)
cargo run -- https://example.com

# Dev loop for the wrap path: spawns `cargo tauri dev` against the user's
# source tree. Reload the webview to pick up source edits — there's no
# watcher (Tauri serves files live from frontendDist).
cargo run -- dev examples/sample-app/index.html

# --- Game project path --------------------------------------------------

# Scaffold a new project (requires Node 18+ and pnpm or npm on PATH)
cargo run -- create demo
cd demo
tau dev                            # Vite + Tauri, with HMR
tau add cannon-es                  # wraps `pnpm add` / `npm add` inside .tau/
tau build -p macos                 # `vite build` -> `cargo tauri build`

# --- Cache ---------------------------------------------------------------

cargo run -- cache size
cargo run -- cache prune --days 30 [--dry-run]
cargo run -- cache clear
```

End-to-end smoke tests:
- Wrap path: `cargo run -- examples/sample-app/index.html` should produce `./build/<bundle>`.
- Game path: `cargo run --release -- create demo && cd demo && ../target/release/tau dev` should open a webview showing a spinning cube.

## External dependencies the binary expects at runtime

- `cargo` and the `tauri` cargo subcommand (`cargo install tauri-cli`) — every build shells out to `cargo tauri ...`.
- `rustup` — used to query and install mobile rustup targets on demand (see [src/build.rs](src/build.rs) `ensure_targets`).
- For mobile targets, the usual Tauri prerequisites (Android SDK/NDK + `ANDROID_HOME`, Xcode for iOS).
- **Game path only**: Node.js 18+ and a JS package manager (pnpm preferred, npm fallback). `tooling.rs` does a preflight check and surfaces a clear error if either is missing.

## Architecture

Each module has a single concern. The dispatch in [src/main.rs](src/main.rs) routes to one of:

- **Cache subcommand** → [src/cache.rs](src/cache.rs)
- **No subcommand** (wrap path) → [src/pipeline.rs](src/pipeline.rs)
- **`create`** → [src/create.rs](src/create.rs)
- **`dev`** → [src/dev.rs](src/dev.rs)
- **`build`** → [src/build_project.rs](src/build_project.rs)
- **`add`** → `tooling::run_add` in [src/tooling.rs](src/tooling.rs)

### The modules

1. **`cli`** ([src/cli.rs](src/cli.rs)) — `clap`-derived `Cli` plus two shared flag groups: `CommonFlags` (`-q`/`-v`, set as `global = true` so they appear on every subcommand) and `BuildFlags` (`--release`, `--name`, `--identifier`, `--output`, `--config`, `--keep-scaffold`). The top-level wrap path uses both flat on `Cli`; the `Build` subcommand reuses `BuildFlags` via `#[command(flatten)]`. `subcommand_negates_reqs` + `args_conflicts_with_subcommands` make the positional `index` optional when a subcommand is given.

2. **`config`** ([src/config/mod.rs](src/config/mod.rs)) — `Config` is the immutable, resolved handle every downstream stage consumes. `resolve(cwd, index_dir, &Overrides)` does three-tier layering: caller `Overrides` > `tau.conf.json` > defaults. `Overrides` is a small explicit struct (not the `Cli`) — the wrap, dev, and build paths each build one from their own args. `apply_project_name_fallback` is shared between `dev` and `build_project` for the "project dir name becomes the app name" rule.

3. **`input`** ([src/input.rs](src/input.rs)) — two things: `Input::parse` classifies a positional argument as a local file (`File { source_root }`) or remote URL, and `discover_project(cwd)` walks up looking for a `.tau/` marker, returning a `ProjectRoot`.

4. **`scaffold`** ([src/scaffold/mod.rs](src/scaffold/mod.rs)) — writes a minimal Tauri v2 project into a tempdir: `src-tauri/{Cargo.toml, tauri.conf.json, build.rs, src/{main,lib}.rs, capabilities/default.json, icons/icon.png}`. Static templates live in [src/scaffold/templates/](src/scaffold/templates/) and game templates (for `tau create`) in [src/scaffold/templates/game/](src/scaffold/templates/game/). Three entry points, one per frontend source:
   - `create_for_source` — `frontendDist` = user's source dir (or materialized copy from `filter.rs`).
   - `create_for_url` — webview's window URL is remote; `frontendDist` is a stub dir Tauri's bundler insists on.
   - `create_for_dev_server` — `devUrl` points at Vite (used by `tau dev` project mode); `frontendDist` is a stub.

5. **`build`** ([src/build.rs](src/build.rs)) — the low-level driver. `TauriCmd` builds `cargo tauri ...` invocations with `CARGO_TARGET_DIR` pointed at `cache::dir()`. `build_platform` runs the build per platform; `extract_artifacts` filters/renames bundles into the output dir. `spawn_dev_desktop` / `spawn_dev_mobile` return a `Child` for the dev path to manage. Shared by both the wrap (pipeline) and the project (build_project) paths.

6. **`pipeline`** ([src/pipeline.rs](src/pipeline.rs)) — orchestrator for the wrap-anything path: parse Input, resolve Config, materialize source tree via `filter`, scaffold, build, extract.

7. **`dev`** ([src/dev.rs](src/dev.rs)) — orchestrator for `tau dev`. Two modes share a single set of helpers (`spawn_and_wait_tauri_dev`, `install_shutdown_flag`, `make_workdir`, `finalize`, `check_status`):
   - **Project mode** (no positional `index`, `discover_project` succeeds): spawn Vite in `.tau/`, wait for `127.0.0.1:1420`, scaffold with `devUrl`, spawn `cargo tauri dev`. On exit (or Ctrl+C) kill both.
   - **Legacy mode** (positional `index` provided): scaffold pointing `frontendDist` at the user's source tree, spawn `cargo tauri dev`. No Vite.

8. **`build_project`** ([src/build_project.rs](src/build_project.rs)) — orchestrator for `tau build`. Discovers a project, runs `vite build`, scaffolds a fresh Tauri project pointing `frontendDist` at `.tau/dist/`, drives `build::build_platform` per requested platform.

9. **`create`** ([src/create.rs](src/create.rs)) — writes the on-disk project tree for `tau create`. Splits user-facing (`src/`, `tau.conf.json`, `.gitignore`) from tooling (`.tau/{package.json, vite.config.js, pnpm-workspace.yaml}`), then runs `<pm> install` inside `.tau/`.

10. **`tooling`** ([src/tooling.rs](src/tooling.rs)) — single owner of every Node/pnpm/npm/vite shell-out. `detect_package_manager`, `ensure_node_present`, `install`, `add`, `vite_dev`, `vite_build`, plus the `run_add` entry point for `tau add`.

11. **`filter`** ([src/filter.rs](src/filter.rs)) — materialize a filtered copy of the user's source tree (used by the wrap path) so `.git/`, `node_modules/`, prior `build/` output, etc. don't end up embedded in the bundle.

12. **`cache`** ([src/cache.rs](src/cache.rs)) — owns the shared `CARGO_TARGET_DIR` path (`~/Library/Caches/tau/target` on macOS, `$XDG_CACHE_HOME/tau/target` elsewhere) and the `size`/`prune`/`clear` operations exposed by the `cache` subcommand.

13. **`log`** ([src/log.rs](src/log.rs)) — tiny structured logger (`Quiet` / `Normal` / `Verbose`).

### Why we don't discover or rewrite assets (wrap path)

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

### Game project on-disk shape (after `tau create my-game`)

```
my-game/
├── src/
│   ├── index.html       # minimal HTML, loads ./game.js as a module
│   ├── game.js          # spinning-cube starter
│   └── assets/          # empty; user drops models/textures here
├── tau.conf.json        # minimal: name + identifier (pinned in source control)
├── .gitignore           # ignores .tau/node_modules/, .tau/dist/, build/
└── .tau/                # hidden plumbing; never edited by the user
    ├── package.json
    ├── vite.config.js
    ├── pnpm-workspace.yaml
    └── node_modules/    # populated by `<pm> install`
```

`pnpm-workspace.yaml` exists to opt out of pnpm 10+'s build-script gate for esbuild (transitively pulled in by Vite). Without it, every install/run exits non-zero. npm ignores the file, so it's safe to write unconditionally.

## Configuration file

Optional `tau.conf.json` next to the index file (or in the cwd, or `--config <path>`) — schema is `FileConfig` in [src/config/mod.rs](src/config/mod.rs). All fields are optional; CLI flags win over file values. `#[serde(deny_unknown_fields)]` is on, so typos in user configs become hard errors. `signing` is parsed and validated by `--release` but not yet wired into the actual `cargo tauri` invocations (release signing is a TODO — the seam is `BuildProfile::Release` + `SigningConfig`).

## Testing

Each module has an in-file `#[cfg(test)] mod tests` block covering the pure functions: `Platform` parsing, identifier slugification, profile-component matching, product-name variant generation, scaffold template invariants, frontendDist wiring, npm-name slugification, `discover_project` walk-up. Run with `cargo test`. The package-manager preflight and Vite spawns are not covered by unit tests — exercise them end-to-end via the smoke tests above.
