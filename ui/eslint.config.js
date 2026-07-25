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

      // "No `any`" (CLAUDE.md §4). `error` rather than `warn`: a tolerated
      // `any` spreads, and the whole point of the typed IPC is lost.
      "@typescript-eslint/no-explicit-any": "error",

      // Frontend mirror of the ban on `println!` on the Rust side.
      "no-console": "error",

      // "Every backend call goes through the typed wrappers in ui/src/ipc/ —
      // never a raw `invoke` in a component" (CLAUDE.md §4). The rule is
      // global; the exception for `src/ipc/` is declared below. Without this
      // guard the IPC boundary is only a review convention, and it eventually
      // gives way.
      "no-restricted-imports": [
        "error",
        {
          patterns: [
            {
              group: ["@tauri-apps/api", "@tauri-apps/api/*", "@tauri-apps/plugin-*"],
              message: "Tauri calls go exclusively through the typed wrappers in src/ipc/.",
            },
          ],
        },
      ],
    },
  },

  // Only the wrapper directory may touch the Tauri API: that is its purpose.
  {
    files: ["src/ipc/**/*.{ts,tsx}"],
    rules: { "no-restricted-imports": "off" },
  },

  // Configuration files run under Node, not inside the WebView.
  {
    files: ["vite.config.ts", "eslint.config.js"],
    languageOptions: { globals: globals.node },
  },

  // Tests may write to the console and handle loosely typed doubles.
  {
    files: ["**/*.test.{ts,tsx}", "src/test/**"],
    rules: { "no-console": "off" },
  },
);
