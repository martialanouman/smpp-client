import js from "@eslint/js";
import globals from "globals";
import tseslint from "typescript-eslint";
import reactHooks from "eslint-plugin-react-hooks";
import reactRefresh from "eslint-plugin-react-refresh";

export default tseslint.config(
  { ignores: ["dist", "coverage", "src/ipc/generated"] },

  {
    files: ["**/*.{ts,tsx}"],
    extends: [js.configs.recommended, ...tseslint.configs.recommended],
    languageOptions: {
      ecmaVersion: 2023,
      globals: globals.browser,
    },
    plugins: {
      "react-hooks": reactHooks,
      "react-refresh": reactRefresh,
    },
    rules: {
      ...reactHooks.configs.recommended.rules,
      "react-refresh/only-export-components": ["warn", { allowConstantExport: true }],

      // « Pas d'`any` » (CLAUDE.md §4). En `error` et non `warn` : un `any`
      // toléré se propage, et le typage fort de l'IPC perd tout son sens.
      "@typescript-eslint/no-explicit-any": "error",

      // Miroir frontend de l'interdiction de `println!` côté Rust.
      "no-console": "error",

      // « Tout appel backend passe par les wrappers typés de ui/src/ipc/ —
      // jamais d'`invoke` brut dans un composant » (CLAUDE.md §4).
      // La règle est globale ; l'exception pour `src/ipc/` est déclarée plus
      // bas. Sans ce garde-fou, la frontière IPC n'est qu'une convention de
      // revue, et elle finit par céder.
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              group: ["@tauri-apps/api", "@tauri-apps/api/*", "@tauri-apps/plugin-*"],
              message: "Les appels Tauri passent exclusivement par les wrappers typés de src/ipc/.",
            },
          ],
        },
      ],
    },
  },

  // Seul le répertoire des wrappers a le droit de toucher l'API Tauri : c'est
  // sa raison d'être.
  {
    files: ["src/ipc/**/*.{ts,tsx}"],
    rules: { "no-restricted-imports": "off" },
  },

  // Les fichiers de configuration tournent sous Node, pas dans la WebView.
  {
    files: ["vite.config.ts", "eslint.config.js"],
    languageOptions: { globals: globals.node },
  },

  // Les tests peuvent produire de la sortie console et manipuler des doubles
  // faiblement typés.
  {
    files: ["**/*.test.{ts,tsx}", "src/test/**"],
    rules: { "no-console": "off" },
  },
);
