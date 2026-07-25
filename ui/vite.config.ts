// `defineConfig` comes from `vitest/config`, not `vite`: that is the variant
// which knows about the `test` block. With the one from `vite`, `tsc --noEmit`
// rejects this configuration — which is exactly what a real typecheck should
// do.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],

  // Tauri drives this server through `beforeDevCommand` and expects it on a
  // fixed port. `strictPort` makes startup fail rather than silently sliding
  // to 1421, which would leave the WebView on a blank page with no error.
  server: {
    port: 1420,
    strictPort: true,
  },

  // Do not clear the terminal: Rust compilation errors surfaced by
  // `tauri dev` would be wiped along with it.
  clearScreen: false,

  // Only these variables cross the boundary into the client bundle. The
  // prefix is a barrier against a development machine's environment variable
  // leaking into a shipped artifact.
  envPrefix: ["VITE_", "TAURI_ENV_"],

  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: false,
  },
});
