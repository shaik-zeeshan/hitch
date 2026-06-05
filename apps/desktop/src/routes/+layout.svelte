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
  //   - the platform shortcuts: command palette (Cmd/Ctrl+K) and settings
  //     toggle (Cmd/Ctrl+,)
  //   - the WKWebView keep-alive heartbeat
  //   - the overlay surfaces (palette + dialogs) that any route may open
  //   - the 3-pane shell (TopNav · LeftRail · Center · RightRail) + rail state
  // Settings (and any future full-window route) renders via children() above
  // the shell; the shell is hidden with display:none so xterm sees a zero-
  // sized host and its ResizeObserver/fit no-ops — preserving the grid for
  // the moment the user returns.
  import "../app.css";
  import { onMount } from "svelte";
  import { goto } from "$app/navigation";
  import { page } from "$app/state";
  import { disposeDaemon, initDaemon } from "$lib/daemon";
  import { initFileDrop } from "$lib/fileDrop";
  import { commandOpen } from "$lib/overlays";
  import { currentDesktopPlatform, isShortcutModifier } from "$lib/desktopPlatform";
  import { initTheme } from "$lib/theme";
  import WindowControls from "$lib/components/WindowControls.svelte";
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
    if (!isShortcutModifier(event, desktopPlatform)) return;
    if (event.key.toLowerCase() === "k") {
      event.preventDefault();
      commandOpen.update((open) => !open);
    } else if (event.key === ",") {
      // Cmd+, (Ctrl+, elsewhere) — the platform-conventional preferences
      // shortcut. Toggles: from the shell it opens /settings, from /settings
      // it returns to the shell (Escape on the page does the same).
      event.preventDefault();
      commandOpen.set(false);
      void goto(page.url.pathname === "/settings" ? "/" : "/settings");
    }
  }

  // Apply the persisted (or default light "paper") theme to <html> and keep it
  // in sync; see theme.ts. This runs during layout init — BEFORE any child
  // mounts — so components that resolve token values at mount (Terminal's
  // xterm theme reads computed colors off <html>) see the correct theme.
  // onMount would be too late: children mount before the parent's onMount.
  initTheme();

  onMount(() => {
    void initDaemon();
    window.addEventListener("keydown", onKeydown);
    // App-wide OS-file-drop listener: drops onto a terminal insert the dropped
    // paths at its prompt (see fileDrop.ts for why this is window-global rather
    // than a per-terminal DOM handler). Registration is async; stash the
    // unlisten so teardown removes it even if the promise resolves after unmount.
    let unlistenDrop: (() => void) | null = null;
    void initFileDrop().then((unlisten) => {
      unlistenDrop = unlisten;
    });
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
      unlistenDrop?.();
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

<!-- Windows is frameless (decorations:false). The caption controls live here,
     OUTSIDE the `.window` shell, so they stay visible on full-window routes that
     hide the shell with display:none (currently /settings) — otherwise Windows
     users would lose every minimize/maximize/close button there. Rendered once
     (a fixed top-right layer) to keep the single native Snap-Layouts overlay
     parked over one max-button rectangle. -->
{#if desktopPlatform === "windows"}
  <WindowControls />
{/if}

<div class="wk-keepalive" aria-hidden="true" style="opacity:{hbOpacity}"></div>

<CommandPalette />
<AddProjectDialog />
<CloneProjectDialog />
<CreateWorktreeDialog />
<RemoveProjectDialog />
<RemoveWorktreeDialog />
<!-- Toasts wear the letterpress chrome: paper-2 fill, ink-0 text, a hairline
     --line border, radius 0. Tokens (var()) resolve live so toasts follow the
     paper/dusk theme switch. The icon accent is the iris primary. -->
<Toaster
  position="bottom-right"
  toastOptions={{
    style:
      "background: var(--paper-2); color: var(--ink-0); border: 1px solid var(--line); border-radius: 0; font-size: 12px; padding: 10px 14px;",
    iconTheme: {
      primary: "var(--iris)",
      secondary: "var(--paper-2)",
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
    grid-template-rows: 42px 1fr;
    background: var(--paper-0);
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
    grid-template-columns: var(--w-left, 295px) 1fr var(--w-right, 330px);
    min-height: 0;
    transition: grid-template-columns 0.2s ease-out;
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
    background: var(--iris);
    pointer-events: none;
  }
</style>
