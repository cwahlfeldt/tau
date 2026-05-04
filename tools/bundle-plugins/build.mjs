import { mkdir, writeFile, readFile, rm } from "node:fs/promises";
import { resolve, dirname } from "node:path";
import { fileURLToPath } from "node:url";
import { $ } from "bun";

const here = dirname(fileURLToPath(import.meta.url));
const outDir = resolve(here, "../../src/scaffold/templates/plugins");
const tmpDir = resolve(here, ".tmp-entries");
await mkdir(outDir, { recursive: true });
await mkdir(tmpDir, { recursive: true });

const plugins = ["fs", "dialog", "haptics"];
const versions = [];

for (const name of plugins) {
  const upper = name.toUpperCase();
  const entry = resolve(tmpDir, `${name}.entry.js`);
  await writeFile(
    entry,
    `import * as plugin from "@tauri-apps/plugin-${name}";\n` +
      `window.__TAURI_PLUGIN_${upper}__ = plugin;\n`,
  );
  const out = resolve(outDir, `${name}.iife.js`);
  await $`bun build ${entry} --target=browser --format=iife --minify --outfile=${out}`;
  const pj = JSON.parse(
    await readFile(
      resolve(here, "node_modules", "@tauri-apps", `plugin-${name}`, "package.json"),
      "utf8",
    ),
  );
  versions.push(`@tauri-apps/plugin-${name}@${pj.version}`);
}

await writeFile(resolve(outDir, "VERSIONS.txt"), versions.join("\n") + "\n");
await rm(tmpDir, { recursive: true });
console.log("bundled:", versions.join(", "));
