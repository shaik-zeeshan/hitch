import adapter from "@sveltejs/adapter-static";
import { vitePreprocess } from "@sveltejs/vite-plugin-svelte";

// SPA mode for the Tauri webview: no SSR/server, a single client-side bundle.
// adapter-static with a `fallback` emits an index.html shell that the webview
// serves for every route (`/`, `/settings`), so SvelteKit routes resolve
// client-side. `paths.relative` defaults to true — correct for Tauri's
// `tauri://localhost` custom protocol.
/** @type {import('@sveltejs/kit').Config} */
export default {
  preprocess: vitePreprocess(),
  kit: {
    adapter: adapter({ fallback: "index.html" }),
  },
};
