<script lang="ts">
  // Unified top nav (mockup .topnav). The macOS window uses titleBarStyle
  // "Overlay" (ADR 0006), so the real traffic lights are drawn by the OS in the
  // top-left — we reserve left padding for them rather than drawing fakes, and
  // mark the bar a drag region so it moves the window.
  import { connection, gitStatus, refreshAll } from "../daemon";
  import { commandOpen } from "../overlays";

  let {
    rightCollapsed = false,
    onToggleLeft,
    onToggleRight,
  }: {
    rightCollapsed?: boolean;
    onToggleLeft: () => void;
    onToggleRight: () => void;
  } = $props();

  const ahead = $derived($gitStatus?.ahead ?? 0);
  const behind = $derived($gitStatus?.behind ?? 0);
</script>

<nav class="topnav" data-tauri-drag-region>
  <button class="iconbtn" title="Toggle sidebar" aria-label="Toggle sidebar" onclick={onToggleLeft}>
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
      ><rect x="1.5" y="2.5" width="13" height="11" rx="2" /><line x1="6" y1="2.5" x2="6" y2="13.5" /></svg
    >
  </button>

  <div class="nav-grow" data-tauri-drag-region></div>

  <button class="cmdk" onclick={() => commandOpen.set(true)} aria-label="Search or jump to">
    <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
      ><circle cx="7" cy="7" r="4.5" /><line x1="10.5" y1="10.5" x2="14" y2="14" /></svg
    >
    <span class="cmdk-label">Search or jump to…</span>
    <span class="k">⌘K</span>
  </button>

  <div class="nav-grow" data-tauri-drag-region></div>

  <div class="nav-right">
    {#if $connection !== "ready"}
      <span class="daemon" class:off={$connection === "offline"} title="Daemon status">
        <span class="dot"></span>
        {$connection === "offline" ? "daemon offline" : "starting daemon…"}
      </span>
      <span class="nav-sep"></span>
    {/if}

    <button class="iconbtn" title="Fetch from origin" aria-label="Fetch" onclick={() => void refreshAll()}>
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
        ><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5V5h-2.5" /></svg
      >
    </button>
    {#if ahead > 0}<span class="ahead" title="{ahead} commits ahead of origin">↑{ahead}</span>{/if}
    {#if behind > 0}<span class="behind" title="{behind} commits behind origin">↓{behind}</span>{/if}

    {#if rightCollapsed}
      <button class="iconbtn" title="Show changes panel" aria-label="Show changes panel" onclick={onToggleRight}>
        <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
          ><rect x="1.5" y="2.5" width="13" height="11" rx="2" /><line x1="10.5" y1="2.5" x2="10.5" y2="13.5" /></svg
        >
      </button>
    {/if}
  </div>
</nav>

<style>
  /* macOS traffic lights are inset to x 16 / y 23 in tauri.conf.json so their
     centers sit on the 44px top-nav row, level with the sidebar toggle icon;
     this reserves their horizontal room. */
  .topnav {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 100%;
    background: var(--bg-2);
    border-bottom: 1px solid var(--line);
    padding: 0 12px 0 78px;
    user-select: none;
  }

  .nav-grow {
    flex: 1;
    align-self: stretch;
  }

  .cmdk {
    display: flex;
    align-items: center;
    gap: 8px;
    flex: 0 1 340px;
    min-width: 120px;
    padding: 5px 8px 5px 10px;
    background: var(--bg-1);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    color: var(--tx-lo);
    font: inherit;
    font-size: 11.5px;
    text-align: left;
    cursor: text;
    transition: border-color var(--t-fast);
  }
  .cmdk:hover {
    border-color: oklch(36% 0.012 265);
  }
  .cmdk svg {
    width: 13px;
    height: 13px;
    color: var(--tx-lo);
    flex: none;
  }
  .cmdk-label {
    flex: 1;
  }
  .cmdk .k {
    font-family: var(--mono);
    font-size: 10px;
    border: 1px solid var(--line);
    border-radius: 4px;
    padding: 1px 5px;
    color: var(--tx-md);
  }

  .nav-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .daemon {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--tx-md);
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--warn);
    box-shadow: 0 0 0 3px oklch(81% 0.13 75 / 0.16);
  }
  .daemon.off {
    color: oklch(78% 0.08 25);
  }
  .daemon.off .dot {
    background: var(--err);
    box-shadow: 0 0 0 3px oklch(68% 0.17 25 / 0.16);
  }
  .nav-sep {
    width: 1px;
    height: 18px;
    background: var(--line);
  }
  .ahead {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--ok);
  }
  .behind {
    font-family: var(--mono);
    font-size: 11px;
    color: var(--warn);
  }

  .iconbtn {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius);
    color: var(--tx-md);
    border: 1px solid transparent;
    background: transparent;
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .iconbtn:hover {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
  .iconbtn svg {
    width: 15px;
    height: 15px;
  }
  .iconbtn:focus-visible {
    outline: 2px solid var(--ac);
    outline-offset: 1px;
  }
</style>
