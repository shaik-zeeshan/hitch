import { defineConfig, type Plugin } from "vitest/config";

// The Agent Registry (sessionDisplay.ts) imports the harness identity marks from
// the `~icons/<collection>/*` virtual modules so each agent entry carries its
// own icon. Those modules only exist under the app's unplugin-icons build, and
// the icon's svelte compiler emits `.svelte` source the node-based test runner
// can't parse. The unit tests never render the marks — they exercise the pure
// title/kind/argv logic — so stub every `~icons/*` import to a placeholder
// component. This keeps importing sessionDisplay cheap without dragging the full
// Svelte build into the fast node config.
const stubVirtualIcons = (): Plugin => ({
  name: "stub-virtual-icons",
  enforce: "pre",
  resolveId(id) {
    return id.startsWith("~icons/") ? `\0virtual-icon:${id}` : null;
  },
  load(id) {
    return id.startsWith("\0virtual-icon:") ? "export default () => null;" : null;
  },
});

// Standalone vitest config: the SvelteKit plugin in vite.config.ts pulls in the
// full app build pipeline, which the pure-module unit tests (byteRing) don't
// need. Keep this minimal and node-based so `vitest run` stays fast and
// hermetic. Scope it to *.test.ts so it never tries to drive Svelte components.
export default defineConfig({
  plugins: [stubVirtualIcons()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
