import { defineConfig } from "vitest/config";

// Standalone vitest config: the SvelteKit plugin in vite.config.ts pulls in the
// full app build pipeline, which the pure-module unit tests (byteRing) don't
// need. Keep this minimal and node-based so `vitest run` stays fast and
// hermetic. Scope it to *.test.ts so it never tries to drive Svelte components.
export default defineConfig({
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
