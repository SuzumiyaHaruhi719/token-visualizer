import { defineConfig } from "vite";

// @ts-expect-error process is a nodejs global
const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  // Multipage build: dashboard (index.html) + pet (pet.html).
  // Input paths are resolved relative to the project root by Vite.
  build: {
    rollupOptions: {
      input: {
        main: "index.html",
        pet: "pet.html",
      },
    },
  },

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
  server: {
    // NOT 1420 — that's Tauri's default and collides with other local Tauri
    // apps (e.g. CorePilot OSD), whose webview would then load THIS app.
    port: 5847,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 5848,
        }
      : undefined,
    watch: {
      // 3. tell Vite to ignore watching `src-tauri`, the Rust build output, and
      //    git worktrees under `.claude/` (agent worktrees carry their own copies
      //    of the project, which would otherwise thrash Vite's watcher and can
      //    crash the dev server).
      ignored: [
        "**/src-tauri/**",
        "**/.claude/**",
        "**/target/**",
        "**/dist/**",
      ],
    },
  },
}));
