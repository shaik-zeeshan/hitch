<script lang="ts">
  // Route `/` — the three-pane triptych shell: the unified top nav over a body
  // of LeftRail · Center · RightRail, mirroring hitch-shell-mockup.html. The
  // cross-route chrome (daemon connection, ⌘K, overlays) lives in the root
  // +layout.svelte; this page owns only the shell layout and its rail-collapse
  // state. xterm terminals mount under Center and unmount on navigation away.
  import TopNav from "$lib/components/TopNav.svelte";
  import LeftRail from "$lib/components/LeftRail.svelte";
  import Center from "$lib/components/Center.svelte";
  import RightRail from "$lib/components/RightRail.svelte";

  let showLeft = $state(true);
  let showRight = $state(true);
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
</style>
