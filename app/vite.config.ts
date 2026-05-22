import { defineConfig } from "vite";
import { svelte } from "@sveltejs/vite-plugin-svelte";

const host = process.env.TAURI_DEV_HOST;

// Voir https://tauri.app/v1/guides/debugging/development-cycle
export default defineConfig(async () => ({
  plugins: [
    svelte({
      compilerOptions: {
        runes: true,
      },
    }),
  ],

  // Empeche Vite de masquer les erreurs Rust
  clearScreen: false,
  // Tauri attend un port fixe et echouera si ce port n'est pas disponible
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      // Tauri travaille avec son propre watch ; on ignore son dossier
      ignored: ["**/src-tauri/**"],
    },
  },

  // Specifications de build
  envPrefix: ["VITE_", "TAURI_ENV_*"],
  build: {
    // Tauri utilise Chromium sur Windows (Edge WebView2) → cible es2021
    target: "es2021",
    minify: !process.env.TAURI_ENV_DEBUG ? "esbuild" : false,
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
  },
}));
