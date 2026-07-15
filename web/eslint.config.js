import js from "@eslint/js";
import reactHooks from "eslint-plugin-react-hooks";
import globals from "globals";
import tseslint from "typescript-eslint";

// ESLint 10 flat config. Lints the app source with typescript-eslint's recommended set plus the
// react-hooks rules (exhaustive-deps guards the transcript/socket effects). The generated OpenAPI
// types and the build output are not linted.
export default tseslint.config(
  // src-tauri is the Rust shell (its target/ holds copied web bundles); schema.ts is generated.
  { ignores: ["dist", "src-tauri", "src/api/schema.ts"] },
  js.configs.recommended,
  tseslint.configs.recommended,
  {
    files: ["**/*.{ts,tsx}"],
    plugins: { "react-hooks": reactHooks },
    languageOptions: {
      ecmaVersion: 2022,
      sourceType: "module",
      globals: globals.browser,
    },
    rules: {
      "react-hooks/rules-of-hooks": "error",
      "react-hooks/exhaustive-deps": "error",
    },
  },
);
