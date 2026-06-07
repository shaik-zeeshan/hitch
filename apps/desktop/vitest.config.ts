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

// `.svelte` component imports are real Svelte source the node-based runner can't
// parse. A few pure-logic modules (fileDrop's UploadToast, appToast's AppToast)
// import a component only to hand it to the toast library as a renderable — the
// unit tests never render it. Stub every `*.svelte` import to an inert component
// so importing those modules stays cheap and hermetic, mirroring the `~icons/*`
// and toast stubs.
const stubSvelteComponents = (): Plugin => ({
  name: "stub-svelte-components",
  enforce: "pre",
  resolveId(id) {
    return id.endsWith(".svelte") ? `\0virtual-svelte:${id}` : null;
  },
  load(id) {
    return id.startsWith("\0virtual-svelte:") ? "export default () => null;" : null;
  },
});

// `svelte-french-toast` ships only browser/svelte export conditions, so vite's
// node resolver can't find an entry for the bare specifier under the node-based
// test config (it throws "No known conditions for '.'"). The toast UI is never
// asserted in the pure-logic unit tests (modules that import it — appToast,
// fileDrop — are exercised for their data path, and tests that care about a
// toast firing `vi.mock` it themselves). Stub the package to an inert toast
// object so importing those modules is cheap and hermetic, mirroring the
// `~icons/*` stub above.
const stubToast = (): Plugin => ({
  name: "stub-svelte-french-toast",
  enforce: "pre",
  resolveId(id) {
    return id === "svelte-french-toast" ? "\0virtual-toast" : null;
  },
  load(id) {
    if (id !== "\0virtual-toast") return null;
    return [
      "const noop = () => '';",
      "const toast = Object.assign(noop, { loading: noop, success: noop, error: noop });",
      "export default toast;",
      "export const Toaster = () => null;",
    ].join("\n");
  },
});

// Standalone vitest config: the SvelteKit plugin in vite.config.ts pulls in the
// full app build pipeline, which the pure-module unit tests (byteRing) don't
// need. Keep this minimal and node-based so `vitest run` stays fast and
// hermetic. Scope it to *.test.ts so it never tries to drive Svelte components.
export default defineConfig({
  plugins: [stubVirtualIcons(), stubSvelteComponents(), stubToast()],
  test: {
    environment: "node",
    include: ["src/**/*.test.ts"],
  },
});
