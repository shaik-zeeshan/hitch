<script lang="ts">
  // Persistent app root. A SvelteKit root layout mounts once and is reused
  // across child-route navigation (`/` ↔ `/settings`), so the daemon connection
  // and global chrome set up here survive navigation — they are NOT torn down
  // when the route changes. The three-pane shell itself lives in the `/` page
  // (+page.svelte); this layout owns only the cross-route concerns:
  //   - the daemon connection lifecycle (initDaemon is idempotent; see daemon.ts)
  //   - the ⌘K command-palette keydown
  //   - the WKWebView keep-alive heartbeat
  //   - the overlay surfaces (palette + dialogs) that any route may open
  import "../app.css";
  import { onMount } from "svelte";
  import { disposeDaemon, initDaemon } from "$lib/daemon";
  import { commandOpen } from "$lib/overlays";
  import CommandPalette from "$lib/components/CommandPalette.svelte";
  import AddProjectDialog from "$lib/components/AddProjectDialog.svelte";
  import CreateWorktreeDialog from "$lib/components/CreateWorktreeDialog.svelte";
  import RemoveProjectDialog from "$lib/components/RemoveProjectDialog.svelte";
  import RemoveWorktreeDialog from "$lib/components/RemoveWorktreeDialog.svelte";
  import { SvelteToast } from "@zerodevx/svelte-toast";

  let { children } = $props();

  // Heartbeat opacity for the keep-alive dot (see below). Toggled on a timer so
  // the WebContent process always has scheduled work to flush.
  let hbOpacity = $state(0.01);

  function onKeydown(event: KeyboardEvent) {
    if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
      event.preventDefault();
      commandOpen.update((open) => !open);
    }
  }

  onMount(() => {
    void initDaemon();
    window.addEventListener("keydown", onKeydown);
    // Keep the macOS WKWebView from going dormant. When the page has no
    // scheduled work (no terminal mounted, or the only terminal is unfocused so
    // xterm's cursor-blink timer is paused), the webview stops flushing frames:
    // clicks and store updates are processed but never painted, so the UI looks
    // frozen until an external event (resize/refresh) wakes it. A low-frequency
    // opacity toggle — the same trick xterm's blinking cursor relies on —
    // guarantees a steady stream of frames so async updates always paint.
    const heartbeat = setInterval(() => {
      hbOpacity = hbOpacity === 0.01 ? 0.02 : 0.01;
    }, 500);
    return () => {
      clearInterval(heartbeat);
      window.removeEventListener("keydown", onKeydown);
      disposeDaemon();
    };
  });
</script>

{@render children()}

<div class="wk-keepalive" aria-hidden="true" style="opacity:{hbOpacity}"></div>

<CommandPalette />
<AddProjectDialog />
<CreateWorktreeDialog />
<RemoveProjectDialog />
<RemoveWorktreeDialog />
<SvelteToast
  options={{
    reversed: true,
    intro: { x: 192 },
    theme: {
      "--toastBackground": "oklch(22% 0.01 265)",
      "--toastColor": "oklch(92% 0.005 265)",
      "--toastBarBackground": "oklch(62% 0.1 265)",
      "--toastBorderRadius": "6px",
      "--toastMsgPadding": "0.55rem 0.9rem",
      "--toastBoxShadow": "0 4px 16px oklch(0% 0 0 / 0.5)",
      "--toastBarHeight": "3px",
      "--toastWidth": "240px",
    },
  }}
/>

<style>
  /* WKWebView keep-alive dot — an imperceptible 1px square whose opacity the
     heartbeat toggles to force a repaint each tick. See onMount above. */
  .wk-keepalive {
    position: fixed;
    top: 0;
    left: 0;
    width: 1px;
    height: 1px;
    background: var(--ac);
    pointer-events: none;
  }
</style>
