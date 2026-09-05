import { defineConfig } from "vite";
import react from "@vitejs/plugin-react-swc";
import path from "path";

const host = process.env.TAURI_DEV_HOST;

// https://vitejs.dev/config/
export default defineConfig(async ({ mode }) => ({
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
      // 3. tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
  plugins: [
    react(),
    // Imported lazily and only in dev. At the top of the file it was loaded on every
    // production build too, for a plugin that never runs there.
    ...(mode === "development"
      ? [(await import("lovable-tagger")).componentTagger()]
      : []),
  ],
  resolve: {
    alias: {
      "@": path.resolve(__dirname, "./src"),
    },
  },
  build: {
    // This only ever runs in WebView2, WKWebView or WebKitGTK, all of which are well past
    // Vite's conservative default target. No downlevelling needed.
    target: process.env.TAURI_ENV_PLATFORM === "windows" ? "chrome105" : "safari13",
    sourcemap: !!process.env.TAURI_ENV_DEBUG,
    // Deliberately no manualChunks: this is a local single-window app loaded off the
    // filesystem, so there is no network waterfall to parallelise. The one split that pays
    // for itself — the locales — comes from import.meta.glob in src/i18n.
  },
}));
