<script lang="ts">
  // Shell root: the unified top nav over a three-pane body
  // (LeftRail · Center · RightRail), mirroring hitch-shell-mockup.html. The
  // floating window chrome from the mockup is dropped — here the shell fills the
  // OS window. Connects to the daemon on mount and tears the listeners down on
  // destroy. Overlay surfaces (⌘K palette + dialogs) live at the root and are
  // opened through the shared overlays store; their context-menu triggers are
  // co-located with the tree/tabs they act on.
  import { onMount } from "svelte";
  import { disposeDaemon, initDaemon } from "./lib/daemon";
  import { commandOpen } from "./lib/overlays";
  import TopNav from "./lib/components/TopNav.svelte";
  import LeftRail from "./lib/components/LeftRail.svelte";
  import Center from "./lib/components/Center.svelte";
  import RightRail from "./lib/components/RightRail.svelte";
  import CommandPalette from "./lib/components/CommandPalette.svelte";
  import AddProjectDialog from "./lib/components/AddProjectDialog.svelte";
  import CreateWorktreeDialog from "./lib/components/CreateWorktreeDialog.svelte";
  import RemoveWorktreeDialog from "./lib/components/RemoveWorktreeDialog.svelte";
  import SettingsDialog from "./lib/components/SettingsDialog.svelte";

  let showLeft = $state(true);
  let showRight = $state(true);

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

<div class="window">
  <TopNav
    rightCollapsed={!showRight}
    onToggleLeft={() => (showLeft = !showLeft)}
    onToggleRight={() => (showRight = !showRight)}
  />

  <div class="body" class:no-left={!showLeft} class:no-right={!showRight}>
    <LeftRail collapsed={!showLeft} />
    <Center />
    <RightRail collapsed={!showRight} onToggleRight={() => (showRight = !showRight)} />
  </div>
</div>

<div class="wk-keepalive" aria-hidden="true" style="opacity:{hbOpacity}"></div>

<CommandPalette />
<AddProjectDialog />
<CreateWorktreeDialog />
<RemoveWorktreeDialog />
<SettingsDialog />

<style>
  .window {
    height: 100%;
    width: 100%;
    display: grid;
    grid-template-rows: 44px 1fr;
    background: var(--bg-1);
    overflow: hidden;
  }

  .body {
    display: grid;
    grid-template-columns: var(--w-left, 250px) 1fr var(--w-right, 356px);
    min-height: 0;
    transition: grid-template-columns var(--t);
  }
  .body.no-left {
    --w-left: 0px;
  }
  .body.no-right {
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
