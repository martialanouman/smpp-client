// `defineConfig` vient de `vitest/config` et non de `vite` : c'est la version
// qui connaît le bloc `test`. Avec celui de `vite`, `tsc --noEmit` rejette la
// configuration — ce qui est précisément ce qu'on attend d'un typecheck réel.
import { defineConfig } from "vitest/config";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

// https://vite.dev/config/
export default defineConfig({
  plugins: [react(), tailwindcss()],

  // Tauri pilote ce serveur via `beforeDevCommand` et s'attend à le trouver
  // sur un port fixe. `strictPort` fait échouer le démarrage plutôt que de
  // glisser silencieusement sur 1421, ce qui laisserait la WebView sur une
  // page blanche sans message d'erreur.
  server: {
    port: 1420,
    strictPort: true,
  },

  // Ne pas effacer la sortie du terminal : les erreurs de compilation Rust
  // remontées par `tauri dev` seraient emportées avec.
  clearScreen: false,

  // Seules ces variables franchissent la frontière vers le bundle client. Le
  // préfixe est une barrière contre la fuite accidentelle d'une variable
  // d'environnement du poste dans un artefact distribué.
  envPrefix: ["VITE_", "TAURI_ENV_"],

  test: {
    globals: true,
    environment: "jsdom",
    setupFiles: ["./src/test/setup.ts"],
    css: false,
  },
});
