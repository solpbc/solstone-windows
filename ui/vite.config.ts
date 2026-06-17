import { defineConfig } from "vite";

// The webview is a pure renderer. Vite builds the static bundle into `dist`,
// which Tauri serves as `frontendDist`. No analytics, no remote sources.
export default defineConfig({
  clearScreen: false,
  server: {
    port: 5173,
    strictPort: true,
  },
  build: {
    outDir: "dist",
    target: "es2021",
    emptyOutDir: true,
  },
});
