<script lang="ts">
  // Left rail (mockup .rail-left): scrollable Projects→Worktrees tree above a
  // pinned footer (Add project · Settings). The brand block lives in the top
  // nav, not here, so the rail opens straight onto the tree.
  import { goto } from "$app/navigation";
  import ProjectTree from "./ProjectTree.svelte";
  import { addProjectOpen } from "../overlays";

  let { collapsed = false }: { collapsed?: boolean } = $props();
</script>

<aside class="rail-left" class:collapsed>
  <div class="rail-scroll">
    <ProjectTree />
  </div>

  <div class="rail-foot">
    <button class="foot-row" onclick={() => addProjectOpen.set(true)}>
      <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
        ><line x1="8" y1="3" x2="8" y2="13" /><line x1="3" y1="8" x2="13" y2="8" /></svg
      >
      Add project
    </button>
    <button class="foot-row" onclick={() => goto("/settings")}>
      <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.2"
        ><circle cx="8" cy="8" r="2.2" /><path
          d="M8 1.5v1.8M8 12.7v1.8M14.5 8h-1.8M3.3 8H1.5M12.6 3.4l-1.3 1.3M4.7 11.3l-1.3 1.3M12.6 12.6l-1.3-1.3M4.7 4.7 3.4 3.4"
        /></svg
      >
      Settings
    </button>
  </div>
</aside>

<style>
  .rail-left {
    background: var(--bg-2);
    border-right: 1px solid var(--line);
    display: grid;
    grid-template-rows: 1fr auto;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    transition: opacity var(--t);
  }
  .rail-left.collapsed {
    opacity: 0;
    pointer-events: none;
  }

  .rail-scroll {
    overflow-y: auto;
    min-height: 0;
  }

  .rail-foot {
    border-top: 1px solid var(--line-soft);
    padding: 7px 8px;
    display: grid;
    gap: 1px;
  }
  .foot-row {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 7px 8px;
    border-radius: var(--radius);
    color: var(--tx-md);
    cursor: pointer;
    font: inherit;
    background: transparent;
    border: 1px solid transparent;
    width: 100%;
    text-align: left;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .foot-row:hover {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
  .foot-row .ico {
    width: 15px;
    height: 15px;
    color: var(--tx-lo);
    flex: none;
  }
</style>
