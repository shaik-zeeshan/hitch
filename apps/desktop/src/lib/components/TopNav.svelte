<script lang="ts">
  // Unified top nav (mockup .topnav). The macOS window uses titleBarStyle
  // "Overlay" (ADR 0006), so the real traffic lights are drawn by the OS in the
  // top-left — we reserve left padding for them rather than drawing fakes, and
  // mark the bar a drag region so it moves the window.
  import {
    daemonReason,
    daemonStatus,
    gitStatus,
    openDaemonLog,
    refreshAll,
    restartDaemon,
  } from "../daemon";
  import { commandOpen } from "../overlays";
  import { currentDesktopPlatform, shortcutLabel } from "../desktopPlatform";

  // Title-bar integration differs per OS (ADR 0006): macOS reserves space for
  // the native Overlay traffic lights on the left; Windows is frameless and
  // draws its own caption controls on the right.
  const platform = currentDesktopPlatform();

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

  const commandPaletteShortcut = shortcutLabel(currentDesktopPlatform(), "K");

  // Daemon Status indicator (ADR 0009). Always a colored dot + a word — never
  // color alone (design principle #3) — and a click-to-open popover carrying the
  // failure reason plus View log / Restart actions.
  const STATUS_LABEL = {
    starting: "starting",
    running: "ready",
    unreachable: "unreachable",
    failed: "failed",
  } as const;
  const statusLabel = $derived(STATUS_LABEL[$daemonStatus]);

  let statusOpen = $state(false);
  let restarting = $state(false);

  async function handleRestart() {
    if (restarting) return;
    restarting = true;
    try {
      await restartDaemon();
      statusOpen = false;
    } finally {
      restarting = false;
    }
  }
</script>

<nav
  class="topnav"
  class:mac={platform === "macos"}
  class:win={platform === "windows"}
  data-tauri-drag-region
>
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
    <span class="k">{commandPaletteShortcut}</span>
  </button>

  <div class="nav-grow" data-tauri-drag-region></div>

  <div class="nav-right">
    <div class="daemon-wrap">
      <button
        class="daemon {$daemonStatus}"
        title="Daemon status"
        aria-haspopup="dialog"
        aria-expanded={statusOpen}
        onclick={() => (statusOpen = !statusOpen)}
      >
        <span class="dot"></span>
        <span class="lbl">{statusLabel}</span>
      </button>
      {#if statusOpen}
        <!-- click-away backdrop closes the popover -->
        <button
          class="sp-backdrop"
          aria-label="Close daemon status"
          onclick={() => (statusOpen = false)}
        ></button>
        <div class="status-pop" role="dialog" aria-label="Daemon status">
          <div class="sp-head">
            <span class="dot {$daemonStatus}"></span>
            <span class="sp-title">Daemon {statusLabel}</span>
          </div>
          {#if $daemonReason}
            <p class="sp-reason">{$daemonReason}</p>
          {:else if $daemonStatus === "running"}
            <p class="sp-reason ok">Connected and responsive.</p>
          {/if}
          <div class="sp-actions">
            <button class="sp-btn" onclick={() => void openDaemonLog()}>View log</button>
            <button class="sp-btn" disabled={restarting} onclick={() => void handleRestart()}>
              {restarting ? "Restarting…" : "Restart daemon"}
            </button>
          </div>
        </div>
      {/if}
    </div>
    <span class="nav-sep"></span>

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
  .topnav {
    display: flex;
    align-items: center;
    gap: 10px;
    height: 100%;
    background: var(--bg-2);
    border-bottom: 1px solid var(--line);
    padding: 0 12px;
    user-select: none;
  }
  /* macOS traffic lights are inset to x 16 / y 23 in tauri.conf.json so their
     centers sit on the 44px top-nav row, level with the sidebar toggle icon;
     this reserves their horizontal room. */
  .topnav.mac {
    padding-left: 78px;
  }
  /* Windows is frameless: the caption controls (WindowControls) are rendered as
     a fixed top-right layer at the layout level (so they stay visible on routes
     that hide the shell, e.g. /settings). The nav reserves their width on the
     right (3 × 46px) so its own content never slides under the controls. */
  .topnav.win {
    padding-right: 138px;
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
  .daemon-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }
  .daemon {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--tx-md);
    background: transparent;
    border: 1px solid transparent;
    border-radius: var(--radius);
    padding: 3px 7px;
    cursor: pointer;
    font: inherit;
    font-size: 11px;
    transition:
      background var(--t-fast),
      border-color var(--t-fast);
  }
  .daemon:hover {
    background: var(--bg-3);
    border-color: var(--line);
  }
  .daemon:focus-visible {
    outline: 2px solid var(--ac);
    outline-offset: 1px;
  }
  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--tx-lo);
    flex: none;
  }
  /* Four-state color (paired with the always-present word label). */
  .daemon.starting,
  .dot.starting {
    color: var(--warn);
  }
  .daemon.starting .dot,
  .dot.starting {
    background: var(--warn);
    box-shadow: 0 0 0 3px oklch(81% 0.13 75 / 0.16);
  }
  .daemon.running {
    color: var(--tx-md);
  }
  .daemon.running .dot,
  .dot.running {
    background: var(--ok);
    box-shadow: 0 0 0 3px oklch(72% 0.16 150 / 0.16);
  }
  .daemon.unreachable,
  .daemon.failed {
    color: oklch(78% 0.08 25);
  }
  .daemon.unreachable .dot,
  .daemon.failed .dot,
  .dot.unreachable,
  .dot.failed {
    background: var(--err);
    box-shadow: 0 0 0 3px oklch(68% 0.17 25 / 0.16);
  }

  .sp-backdrop {
    position: fixed;
    inset: 0;
    z-index: 40;
    background: transparent;
    border: 0;
    cursor: default;
  }
  .status-pop {
    position: absolute;
    top: calc(100% + 8px);
    right: 0;
    z-index: 41;
    width: 280px;
    background: var(--bg-2);
    border: 1px solid var(--line);
    border-radius: var(--radius);
    box-shadow: 0 8px 28px oklch(0% 0 0 / 0.4);
    padding: 12px;
    display: flex;
    flex-direction: column;
    gap: 9px;
  }
  .sp-head {
    display: flex;
    align-items: center;
    gap: 7px;
  }
  .sp-title {
    font-size: 12px;
    font-weight: 600;
    color: var(--tx-hi);
    text-transform: capitalize;
  }
  .sp-reason {
    font-size: 11px;
    line-height: 1.5;
    color: var(--tx-md);
    font-family: var(--mono);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 160px;
    overflow-y: auto;
    margin: 0;
  }
  .sp-reason.ok {
    font-family: var(--ui);
    color: var(--tx-lo);
  }
  .sp-actions {
    display: flex;
    gap: 6px;
  }
  .sp-btn {
    flex: 1;
    font: inherit;
    font-size: 11px;
    padding: 5px 8px;
    border-radius: var(--radius);
    border: 1px solid var(--line);
    background: var(--bg-3);
    color: var(--tx-md);
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .sp-btn:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .sp-btn:disabled {
    opacity: 0.5;
    cursor: default;
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
