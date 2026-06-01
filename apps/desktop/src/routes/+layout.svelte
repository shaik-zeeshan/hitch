<script lang="ts">
  // Persistent app root. A SvelteKit root layout mounts once and is reused
  // across child-route navigation (`/` ↔ `/settings`), so the daemon connection
  // AND the three-pane shell set up here survive navigation — they are NOT
  // torn down when the route changes. Hosting the shell at the layout level
  // (rather than inside the `/` +page) is what keeps every live xterm and its
  // PTY-aligned grid intact when the user pops over to /settings and back;
  // remounting under /+page would re-parse the byte ring against a fresh
  // xterm whose size may not match the PTY's current cols/rows, displacing
  // wrapped lines and cursor-addressed TUI output.
  //
  // This layout owns:
  //   - the daemon connection lifecycle (initDaemon is idempotent; see daemon.ts)
  //   - the platform command-palette shortcut (Cmd+K on macOS, Ctrl+K elsewhere)
  //   - the WKWebView keep-alive heartbeat
  //   - the overlay surfaces (palette + dialogs) that any route may open
  //   - the 3-pane shell (TopNav · LeftRail · Center · RightRail) + rail state
  // Settings (and any future full-window route) renders via children() above
  // the shell; the shell is hidden with display:none so xterm sees a zero-
  // sized host and its ResizeObserver/fit no-ops — preserving the grid for
  // the moment the user returns.
  import "../app.css";
  import { onMount } from "svelte";
  import { page } from "$app/state";
  import { disposeDaemon, initDaemon } from "$lib/daemon";
  import { commandOpen } from "$lib/overlays";
  import { currentDesktopPlatform, isShortcutModifier } from "$lib/desktopPlatform";
  import CommandPalette from "$lib/components/CommandPalette.svelte";
  import AddProjectDialog from "$lib/components/AddProjectDialog.svelte";
  import CloneProjectDialog from "$lib/components/CloneProjectDialog.svelte";
  import CreateWorktreeDialog from "$lib/components/CreateWorktreeDialog.svelte";
  import RemoveProjectDialog from "$lib/components/RemoveProjectDialog.svelte";
  import RemoveWorktreeDialog from "$lib/components/RemoveWorktreeDialog.svelte";
  import TopNav from "$lib/components/TopNav.svelte";
  import LeftRail from "$lib/components/LeftRail.svelte";
  import Center from "$lib/components/Center.svelte";
  import RightRail from "$lib/components/RightRail.svelte";
  import { Toaster } from "svelte-french-toast";

  let { children } = $props();

  // Rail collapse state lives here (not in /+page.svelte) so a navigation to
  // /settings and back doesn't snap collapsed rails back to expanded — which
  // would also change the center column's width and invalidate the live
  // xterm grids.
  let showLeft = $state(true);
  let showRight = $state(true);

  // Routes that fully replace the shell (the settings page renders on top of
  // the layout while the shell is hidden). Keep this allowlist tight — any
  // future overlay route must opt in explicitly so daemon-driven views never
  // tear down the warm terminal cache by accident.
  const SHELL_HIDDEN_ROUTES = new Set(["/settings"]);
  const shellHidden = $derived(SHELL_HIDDEN_ROUTES.has(page.url.pathname));

  // Heartbeat opacity for the keep-alive dot (see below). Toggled on a timer so
  // the WebContent process always has scheduled work to flush.
  let hbOpacity = $state(0.01);
  const desktopPlatform = currentDesktopPlatform();


  function onKeydown(event: KeyboardEvent) {
    if (isShortcutModifier(event, desktopPlatform) && event.key.toLowerCase() === "k") {
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

<!-- The shell is mounted exactly once for the app's lifetime. When a route
     opts into replacing it (currently only /settings), we hide it via the
     `.shell-hidden` class — `display:none` strips the host of measurable
     area so the Terminal's ResizeObserver/fit guards no-op (no grid changes),
     and the warm xterm + WebGL renderer continues to consume PTY output in
     place. The moment the user navigates back, the shell becomes visible
     again at exactly the size it had on the way out. -->
<div class="window" class:no-left={!showLeft} class:no-right={!showRight} class:shell-hidden={shellHidden} aria-hidden={shellHidden}>
  <TopNav
    rightCollapsed={!showRight}
    onToggleLeft={() => (showLeft = !showLeft)}
    onToggleRight={() => (showRight = !showRight)}
  />

  <div class="body">
    <LeftRail collapsed={!showLeft} />
    <Center />
    <RightRail collapsed={!showRight} onToggleRight={() => (showRight = !showRight)} />
  </div>
</div>

{@render children()}

<div class="wk-keepalive" aria-hidden="true" style="opacity:{hbOpacity}"></div>

<CommandPalette />
<AddProjectDialog />
<CloneProjectDialog />
<CreateWorktreeDialog />
<RemoveProjectDialog />
<RemoveWorktreeDialog />
<Toaster
  position="bottom-right"
  toastOptions={{
    style:
      "background: oklch(20% 0.015 265); color: oklch(90% 0.005 265); border: 1px solid oklch(30% 0.02 265); font-size: 12px; padding: 10px 14px;",
    iconTheme: {
      primary: "oklch(62% 0.1 265)",
      secondary: "oklch(20% 0.015 265)",
    },
  }}
/>

<style>
  /* The 3-pane shell. Lives at the layout level (not the route) so the live
     xterm instances inside Center survive navigation to overlay routes — see
     the script-level note above. Layout/visual rules are unchanged from the
     previous /+page.svelte; only the mount point moved. */
  .window {
    height: 100%;
    width: 100%;
    display: grid;
    grid-template-rows: 44px 1fr;
    background: var(--bg-1);
    overflow: hidden;
  }
  /* `display:none` collapses the shell out of layout entirely so a route
     rendered above it (children() — e.g. /settings) gets the full viewport.
     xterm's host measures zero while hidden, so ResizeObserver/fit no-op and
     no daemon resize fires; the terminal grid is identical on return. */
  .window.shell-hidden {
    display: none;
  }
  .body {
    display: grid;
    grid-template-columns: var(--w-left, 250px) 1fr var(--w-right, 356px);
    min-height: 0;
    transition: grid-template-columns var(--t);
  }
  .window.no-left .body {
    --w-left: 0px;
  }
  .window.no-right .body {
    --w-right: 0px;
  }

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
