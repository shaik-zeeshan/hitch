import { defineConfig } from "vite";
import { sveltekit } from "@sveltejs/kit/vite";
import tailwindcss from "@tailwindcss/vite";
import Icons from "unplugin-icons/vite";
import { FileSystemIconLoader } from "unplugin-icons/loaders";

const host = process.env.TAURI_DEV_HOST;

// https://vite.dev/config/
export default defineConfig(async () => ({
  plugins: [
    tailwindcss(),
    sveltekit(),
    // Icons are imported as Svelte components from the `~icons/<collection>/<name>`
    // virtual module and compiled to inline SVG at build time (no runtime, no
    // network requests). Generic UI icons come from the Lucide collection
    // (`~icons/lucide/*`); the harness identity marks (Claude/Codex/shell) are a
    // local custom collection loaded from src/lib/icons (`~icons/hitch/*`).
    Icons({
      compiler: "svelte",
      customCollections: {
        hitch: FileSystemIconLoader("./src/lib/icons"),
      },
    }),
  ],

  // Vite options tailored for Tauri development and only applied in `tauri dev` or `tauri build`
  //
  // 1. prevent Vite from obscuring rust errors
  clearScreen: false,
  // 2. tauri expects a fixed port, fail if that port is not available
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
      // 3. tell Vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
}));
