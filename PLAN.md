# Tau as a game engine: Vite-backed projects with hidden tooling

## Context

Today `tau <index.html>` wraps any static web app into a Tauri binary. The design ("`frontendDist` points at the user's source dir, no asset rewriting") works, and the [examples/blade-dash/](examples/blade-dash/) project proves three.js games ship through it. But to _start_ a game, the user has to hand-write `index.html`, manually drop three.js into `lib/`, maintain an importmap, and remember the index path on every command. There's no dependency story — adding `cannon-es` or any other lib means finding a CDN copy and copying files.

The user wants a real engine experience:

```
tau create my-game        # one command, ready to dev
cd my-game
$EDITOR src/game.js       # edit
tau dev                   # HMR, no positional arg
tau add cannon-es         # `npm install` under the hood
tau build -p macos        # production bundle + Tauri build
```

Confirmed product decisions (from clarifying questions):

- **Optional `tau.conf.json`**: only present if the user wants to override defaults or configure release/signing. Bare `tau create` produces no config file.
- **Hidden tooling in `.tau/`**: `package.json`, `vite.config.js`, `node_modules/`, lockfile all live in `.tau/`. The user never edits anything in there.
- **Vite is the bundler**: dev = Vite dev server + Tauri webview pointed at it (HMR works). Build = `vite build` → `cargo tauri build` reading from `.tau/dist`.
- **Deps via `tau add`**: wraps `pnpm add` (or fallback `npm install`) running inside `.tau/`.
- **User layout**: `src/index.html`, `src/game.js`, `src/assets/...`. Optional `tau.conf.json` at root.

This is a meaningful architectural shift: tau gains a JS toolchain dependency (Node.js + a package manager). The trade-off is intentional — it's the only way to get `tau add three-mesh-bvh` to "just work" without us reinventing dependency resolution.

## Project shape after `tau create my-game`

```
my-game/
├── src/
│   ├── index.html         # minimal HTML; loads /src/game.js as a module
│   ├── game.js            # user's entry — `import * as THREE from 'three'`
│   └── assets/            # empty; user drops models/textures here
├── tau.conf.json          # OPTIONAL — not written by `tau create`. Only created when the user runs `tau init` or hand-writes one.
└── .tau/                  # hidden plumbing; never edited by the user
    ├── package.json       # has `three` already; `tau add X` adds more
    ├── vite.config.js     # `root: '../src'`, `build.outDir: '../.tau/dist'`
    ├── pnpm-lock.yaml     # or package-lock.json — depends on chosen package manager
    └── node_modules/      # populated by `tau create` and `tau add`
```

`src/index.html` template — minimal, no importmap (Vite handles bare imports):

```html
<!doctype html>
<html>
  <head>
    <meta charset="utf-8" />
    <title>{name}</title>
    <style>
      body {
        margin: 0;
        overflow: hidden;
      }
      canvas {
        display: block;
      }
    </style>
  </head>
  <body>
    <script type="module" src="./game.js"></script>
  </body>
</html>
```

`src/game.js` template — minimal spinning cube so the user immediately sees something:

```js
import * as THREE from "three";

const renderer = new THREE.WebGLRenderer({ antialias: true });
renderer.setSize(innerWidth, innerHeight);
document.body.appendChild(renderer.domElement);

const scene = new THREE.Scene();
const camera = new THREE.PerspectiveCamera(
  70,
  innerWidth / innerHeight,
  0.1,
  1000,
);
camera.position.z = 3;

const cube = new THREE.Mesh(
  new THREE.BoxGeometry(),
  new THREE.MeshNormalMaterial(),
);
scene.add(cube);

renderer.setAnimationLoop(() => {
  cube.rotation.x += 0.01;
  cube.rotation.y += 0.01;
  renderer.render(scene, camera);
});

addEventListener("resize", () => {
  camera.aspect = innerWidth / innerHeight;
  camera.updateProjectionMatrix();
  renderer.setSize(innerWidth, innerHeight);
});
```

## Implementation

### 1. New module: `src/tooling.rs`

Owns everything to do with Node and the package manager. Single concern: shell out cleanly.

```rust
pub fn detect_package_manager() -> Result<PackageManager>;       // pnpm > npm; error if neither
pub fn ensure_node_present() -> Result<()>;                      // `node --version`; error with install URL if missing
pub fn install(tau_dir: &Path, log: &Logger) -> Result<()>;       // runs `pnpm install` inside .tau
pub fn add(tau_dir: &Path, pkg: &str, log: &Logger) -> Result<()>;// runs `pnpm add <pkg>` inside .tau
pub fn vite_dev(tau_dir: &Path, log: &Logger) -> Result<Child>;   // `pnpm vite` (returns child for caller to manage)
pub fn vite_build(tau_dir: &Path, log: &Logger) -> Result<()>;    // `pnpm vite build`
```

Prefer `pnpm` (smaller `node_modules`, faster), fall back to `npm` if pnpm isn't installed. Don't try to install pnpm on the user's behalf — just inform them.

### 2. New module: `src/create.rs`

```rust
pub fn run(name: String, log: &Logger) -> Result<()>
```

1. Validate `name`: non-empty, no path separators.
2. `target = cwd.join(&name)`. Error if exists and non-empty.
3. Check Node present (`tooling::ensure_node_present`).
4. Detect package manager.
5. Create directory tree: `target/src/assets`, `target/.tau`.
6. Write embedded templates:
   - `src/index.html` (with `{name}` substituted via [scaffold::render](src/scaffold/mod.rs#L129))
   - `src/game.js`
   - `.tau/package.json` (with `three` as a dep, `vite` as dev dep, pinned versions)
   - `.tau/vite.config.js`
   - `.gitignore` at root (ignores `.tau/node_modules/`, `.tau/dist/`, `build/`, `.DS_Store`)
7. Run `pnpm install` (or `npm install`) inside `.tau/`. Stream output through the logger.
8. Print next steps: `cd <name> && tau dev`.

The user does NOT need to run `npm install` themselves — `tau create` does it. The first dev/build is then instant (no install pause).

### 3. New templates: `src/scaffold/templates/game/`

Embedded via `include_str!`/`include_bytes!`:

```
src/scaffold/templates/game/
├── index.html.tmpl              # has {name}
├── game.js                      # static
├── package.json.tmpl            # has {name}, pinned three + vite versions
├── vite.config.js               # static
└── gitignore                    # static, written as `.gitignore`
```

Vite config:

```js
import { defineConfig } from "vite";
import { resolve } from "path";

export default defineConfig({
  root: resolve(__dirname, "../src"),
  base: "./", // critical for Tauri's file:// prod loads
  build: {
    outDir: resolve(__dirname, "dist"),
    emptyOutDir: true,
    target: "esnext", // Tauri webview is modern
  },
  server: {
    port: 1420,
    strictPort: true, // fail if taken; matches Tauri's expectation
    host: "127.0.0.1",
  },
  clearScreen: false, // don't clobber tauri's logs
});
```

Pinned versions to start: `three@^0.170.0`, `vite@^5.4.0`. Bump when needed; keep them in one place (the template).

### 4. CLI surface — [src/cli.rs](src/cli.rs)

Add three new subcommands. **Keep the existing `tau <index.html>` form working unchanged** — there are non-game uses (wrapping a remote URL, wrapping any static site).

```rust
enum Command {
    Cache { ... },                       // unchanged
    Dev { ... },                         // unchanged signature, but body changes (see §6)
    Create { name: String, ... },        // NEW
    Build { ... },                       // NEW: project-aware (no positional arg)
    Add { package: String, ... },        // NEW: tau add <pkg>
    Init { ... },                        // NEW: writes a default tau.conf.json into a project that lacks one
}
```

`Dev` and `Build` both detect the project from cwd via `discover_project()` (see §5). The legacy `tau <index>` no-subcommand form stays — it routes through today's `pipeline::run` for the wrap-arbitrary-html case.

**Why both `tau <index>` AND `tau build`**: they serve different users. `tau <index>` wraps anything. `tau build` is for projects created by `tau create` (cwd-aware, runs Vite first). The two paths diverge at the entry point and share the underlying Tauri scaffold/build code.

### 5. Project discovery — `src/input.rs` addition

```rust
pub struct ProjectRoot {
    pub root: PathBuf,        // dir containing .tau/
    pub src_dir: PathBuf,     // root/src
    pub tau_dir: PathBuf,     // root/.tau
    pub dist_dir: PathBuf,    // root/.tau/dist
}

pub fn discover_project(cwd: &Path) -> Option<ProjectRoot>
```

Walks up from `cwd` looking for a `.tau/` directory (the marker — more reliable than `tau.conf.json` since that's optional). On hit, returns paths.

### 6. `tau dev` rewrite — [src/dev.rs](src/dev.rs)

Two paths inside `dev::run`:

- **Project mode** (no `index` arg, `discover_project` succeeds):
  1. `tooling::ensure_node_present`.
  2. Spawn `vite` in `.tau/` → child process A.
  3. Wait until `127.0.0.1:1420` responds (TCP probe loop, ~5s timeout).
  4. Generate Tauri scaffold in tempdir with `tauri.conf.json` carrying:
     - `build.frontendDist`: a stub dir (Tauri requires this to exist at build time even when devUrl is set).
     - `build.devUrl`: `"http://127.0.0.1:1420"` — this is what tells Tauri to point the webview at Vite during dev.
  5. Spawn `cargo tauri dev` → child process B.
  6. Wait on B; on B exit, kill A.
- **Legacy mode** (positional `index` provided OR no project found): today's behavior unchanged.

The `tauri.conf.json` builder in [src/scaffold/mod.rs:137](src/scaffold/mod.rs#L137) needs a new variant — call it `FrontendSource::DevServer { url, stub_dir }` — alongside the existing `Local` and `Url`. (The `Url` variant means "remote webview"; `DevServer` means "local Vite, scaffold owns the stub dir.")

### 7. `tau build` — new module: `src/build_project.rs`

(Name avoids clashing with [src/build.rs](src/build.rs), which stays as the lower-level "drive cargo tauri" module.)

```rust
pub fn run(args: BuildArgs) -> Result<()>
```

1. `discover_project` — error if not found ("Run `tau build` from inside a tau project, or use `tau <path/to/index.html>` to wrap an arbitrary file.").
2. `tooling::ensure_node_present`.
3. Run `pnpm install` if `.tau/node_modules` is missing or stale (compare lockfile mtime).
4. Run `vite build` → produces `.tau/dist/` with hashed assets, tree-shaken.
5. Resolve config: load `tau.conf.json` if present; otherwise use sensible defaults derived from project dir name. (Reuse [config::resolve](src/config/mod.rs#L116) but adapt to "no index" — pass `index_dir = Some(project.root)` so `tau.conf.json` is discovered correctly.)
6. Generate Tauri scaffold pointing `frontendDist` at `.tau/dist/` (absolute path).
7. Drive `build::build_platform` and `build::extract_artifacts` — same code path used today, no changes needed there.

### 8. `tau add <pkg>` — small wrapper

```rust
pub fn run(package: String, log: &Logger) -> Result<()>
```

1. `discover_project` — error if not found.
2. `tooling::add(&project.tau_dir, &package, log)`.

That's it. Pkg manager handles deduplication, version resolution, lockfile.

### 9. `tau init` — for users who want config

```rust
pub fn run(log: &Logger) -> Result<()>
```

1. `discover_project` — error if not found.
2. Error if `tau.conf.json` already exists (don't clobber).
3. Write a commented template `tau.conf.json` showing every available knob (name, identifier, version, build.platforms, signing). User uncomments what they want.

### 10. Scaffold changes — [src/scaffold/mod.rs](src/scaffold/mod.rs)

- Add `FrontendSource::DevServer { url: &str, stub_dir: &Path }` variant.
- In `tauri_conf`: when source is `DevServer`, set `build.devUrl` AND `build.frontendDist` (to the stub). Existing tests for Local/Url variants stay green; add new tests for DevServer.
- Promote `render`, `write_text`, `write_bytes` to `pub(crate)` so `create.rs` can use them.

### 11. Wiring — [src/main.rs](src/main.rs)

Dispatch new commands. Each `tau <cmd>` constructs its args struct and calls into the relevant module.

## Critical files

**Modify:**

- [src/cli.rs](src/cli.rs) — add Create/Build/Add/Init commands.
- [src/main.rs](src/main.rs) — dispatch.
- [src/dev.rs](src/dev.rs) — split into project mode vs legacy mode.
- [src/scaffold/mod.rs](src/scaffold/mod.rs) — new `FrontendSource::DevServer`, expose helpers.
- [src/input.rs](src/input.rs) — add `discover_project`.

**New:**

- `src/create.rs`
- `src/tooling.rs`
- `src/build_project.rs`
- `src/scaffold/templates/game/{index.html.tmpl, game.js, package.json.tmpl, vite.config.js, gitignore}`

**Untouched (key — these are stable):**

- [src/build.rs](src/build.rs) — the cargo-tauri driver. Both legacy wrap and new project-build path use it.
- [src/config/mod.rs](src/config/mod.rs) — resolution stays identical; `tau.conf.json` semantics are unchanged. We only relax the requirement that one exists (already done — see line 192, returns default when no file found).
- [src/cache.rs](src/cache.rs), [src/filter.rs](src/filter.rs), [src/log.rs](src/log.rs).

## Reused functions (do NOT reimplement)

- [config::default_identifier](src/config/mod.rs#L219) — name → `com.tau.<slug>`.
- [config::resolve](src/config/mod.rs#L116) — three-tier config; already returns sensible defaults when no `tau.conf.json` exists.
- [scaffold::render / write_text / write_bytes](src/scaffold/mod.rs#L113-L135).
- [build::build_platform / extract_artifacts](src/build.rs#L58) — drive Tauri unchanged.
- [build::ensure_targets](src/build.rs#L19) — rustup target install.

## Verification

1. **Unit tests** (`cargo test`):
   - `create::run` produces the expected file tree (tempdir-backed). Don't run `npm install` in tests; mock the tooling layer or skip with `#[ignore]`.
   - `discover_project` finds `.tau/` in cwd, in a parent, returns None when absent.
   - `tauri_conf` with `FrontendSource::DevServer` sets `devUrl` and `frontendDist`.
   - Existing tests in scaffold/config/build all still pass — no behavior change to legacy paths.

2. **End-to-end smoke** (manual; document in CLAUDE.md):

   ```bash
   cargo run --release -- create demo
   cd demo
   ../target/release/tau dev      # webview opens, shows spinning cube
   # edit src/game.js (e.g. change cube color), webview HMR-updates without reload
   ../target/release/tau add cannon-es
   # confirm node_modules/cannon-es exists
   ../target/release/tau build -p macos
   # confirm build/demo.app appears
   ```

3. **Backward compat**: `cargo run -- examples/sample-app/index.html` and `cargo run -- dev examples/sample-app/index.html` continue to work unchanged. Verify with the existing smoke test described in CLAUDE.md.

## Open questions for later (not blocking)

- **Hot rust reload**: code changes to `src-tauri/src/lib.rs` aren't relevant since users don't write Rust in this design. We could later expose a `tau plugin add fs` that adds Tauri plugins to `Cargo.toml.tmpl`. Out of scope now.
- **Package manager preference flag**: should `tau create --pm npm` exist? Skip for v1; auto-detect.
- **Asset pipeline**: Vite handles JS/CSS, but model files (`.gltf`, `.glb`) and textures should also live under `src/` and be copied through. Vite does this by default for assets referenced via `new URL('./asset.glb', import.meta.url)`. Document this pattern in the starter `game.js` comment.
- **Updating tau itself shouldn't strand existing projects**: pinning `vite` and `three` in the template means `tau create` always writes the version this binary was built with. Existing projects keep their pinned versions in `.tau/package.json`. No upgrade story yet — explicitly fine for v1.

## Out of scope

- Multiple project templates (`--template 2d-canvas`, etc.). Three.js only for now.
- Hot Rust reload, custom Tauri plugins, signing/notarization.
- Replacing the existing `tau <index>` wrap path — stays exactly as-is.
- Asset preprocessing (texture compression, model optimization).
