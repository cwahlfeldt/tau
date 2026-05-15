import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";

// Build into ./dist so `tau examples/r3f-demo/dist/` picks it up.
// `base: "./"` produces relative asset URLs — required by Tauri's
// frontendDist (no absolute /assets/... paths).
export default defineConfig({
  root: "src",
  base: "./",
  build: {
    outDir: "../dist",
    emptyOutDir: true,
  },
  plugins: [react()],
});
