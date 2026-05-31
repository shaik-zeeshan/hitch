<script lang="ts">
  // Left rail (mockup .rail-left): scrollable Projects→Worktrees tree above a
  // pinned footer (Add project · Settings). The brand block lives in the top
  // nav, not here, so the rail opens straight onto the tree.
  //
  // "Add project" is a split control: the primary click opens the native folder
  // picker directly (the common case — add a local repo/folder). The chevron
  // menu keeps that fast path, adds an explicit manual local-path fallback, and
  // leaves remote clone in its own dialog.
  import { goto } from "$app/navigation";
  import { DropdownMenu } from "bits-ui";
  import ProjectTree from "./ProjectTree.svelte";
  import { pickAndAddProject } from "../daemon";
  import { addProjectOpen, cloneProjectOpen } from "../overlays";

  let { collapsed = false }: { collapsed?: boolean } = $props();
</script>

<aside class="rail-left" class:collapsed>
  <div class="rail-scroll">
    <ProjectTree />
  </div>

  <div class="rail-foot">
    <div class="add-split">
      <button class="foot-row add-main" onclick={() => void pickAndAddProject()}>
        <svg class="ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
          ><line x1="8" y1="3" x2="8" y2="13" /><line x1="3" y1="8" x2="13" y2="8" /></svg
        >
        Add project
      </button>
      <DropdownMenu.Root>
        <DropdownMenu.Trigger>
          {#snippet child({ props })}
            <button {...props} class="add-more" aria-label="More add options" title="More add options">
              <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"
                ><path d="M4 6l4 4 4-4" /></svg
              >
            </button>
          {/snippet}
        </DropdownMenu.Trigger>
        <DropdownMenu.Portal>
          <DropdownMenu.Content class="menu" align="end" side="top" sideOffset={6}>
            <DropdownMenu.Item class="mi" onSelect={() => void pickAndAddProject()}>
              <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M1.5 4.5a2 2 0 0 1 2-2h3l1.5 1.6h4.5a2 2 0 0 1 2 2v5.4a2 2 0 0 1-2 2h-9a2 2 0 0 1-2-2z" /></svg
              >
              Add local folder…
            </DropdownMenu.Item>
            <DropdownMenu.Item class="mi" onSelect={() => addProjectOpen.set(true)}>
              <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                ><path d="M2.5 3.5h11v9h-11z" /><path d="M4.5 6.25h7M4.5 8h5M4.5 9.75h4" /></svg
              >
              Enter local path…
            </DropdownMenu.Item>
            <DropdownMenu.Item class="mi" onSelect={() => cloneProjectOpen.set(true)}>
              <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle cx="12" cy="5" r="1.6" /><path
                  d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11"
                /></svg
              >
              Clone remote repository…
            </DropdownMenu.Item>
          </DropdownMenu.Content>
        </DropdownMenu.Portal>
      </DropdownMenu.Root>
    </div>
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

  /* split "Add project": primary action + a chevron that opens the add menu */
  .add-split {
    display: flex;
    align-items: stretch;
    gap: 1px;
  }
  .add-split .add-main {
    flex: 1;
    min-width: 0;
  }
  .add-more {
    display: grid;
    place-items: center;
    width: 30px;
    flex: none;
    padding: 0;
    border-radius: var(--radius);
    border: 1px solid transparent;
    background: transparent;
    color: var(--tx-lo);
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .add-more svg {
    width: 12px;
    height: 12px;
  }
  .add-more:hover,
  .add-more[data-state="open"] {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
</style>
