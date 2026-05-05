# configured-app

A small example showing what [`tau.conf.json`](tau.conf.json) can do for
you so you don't have to keep re-typing `--name` / `--identifier` /
`-p macos,linux` / etc. on every invocation.

## What this demonstrates

| Field | What it does | Where you see it |
| --- | --- | --- |
| `name` | Sets `productName` in the bundle | window title; `app.getName()` |
| `version` | Bundle version | `app.getVersion()` |
| `identifier` | Bundle ID (must be reverse-DNS) | `app.getIdentifier()`; APK/`.app` metadata |
| `include` | Glob patterns of extra files to bundle into `dist/` even though no `<script>`/`<link>`/`<img>` references them | `data/notes.json` is fetched at runtime |
| `build.output` | Where built artifacts land | `./out/` instead of the default `./build/` |
| `build.platforms` | Default platforms when `-p` isn't given | header logs `platforms: macos, linux` |

## Run it

From inside this directory:

```bash
# Hot-reload dev loop. Pulls config from ./tau.conf.json automatically.
tau dev index.html

# Bundle for the platforms named in tau.conf.json (macos + linux),
# artifacts go to ./out/ (also from tau.conf.json).
tau index.html

# Override platforms or output ad-hoc — CLI flags always win.
tau index.html -p macos --output ./tmp-build
```

## What you should see

The window opens to "Configured Demo" (the `name` from the conf), shows
the resolved identity via the Tauri JS API, and lists three lines from
`data/notes.json` — proving the `include` glob shipped a file that the
HTML doesn't reference.

## How the override layering works

CLI flag > `tau.conf.json` > built-in defaults. So:

```bash
tau index.html --name "Override"     # ignores name in conf
tau index.html                       # uses conf's "Configured Demo"
```

Unknown fields in `tau.conf.json` are hard errors (`deny_unknown_fields`),
so typos are caught at parse time, not at build time.
