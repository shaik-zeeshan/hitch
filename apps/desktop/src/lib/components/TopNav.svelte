<script lang="ts">
  // Paper Terminal top bar (doc-design/structure.md "Top bar"). One 42px bar,
  // three zones: OS traffic lights (left, drawn by the macOS Overlay titlebar —
  // ADR 0006, we only reserve their room), the window-centered command palette
  // trigger, and a right cluster (settings + theme toggle + daemon indicator).
  // The whole bar is the window drag region; interactive children opt out via
  // no-drag.
  //
  // There is NO app name, breadcrumb, or git ahead/behind status here — git
  // sync moved to the right rail. The left/right rail toggle buttons were also
  // removed (the design's bar has three zones only); the collapse props/
  // callbacks are kept so the layout's machinery still compiles and the buttons
  // can return elsewhere later.
  import { goto } from "$app/navigation";
  import {
    daemonReason,
    daemonStatus,
    openDaemonLog,
    restartDaemon,
  } from "../daemon";
  import { commandOpen } from "../overlays";
  import { currentDesktopPlatform, shortcutKeys, shortcutLabel } from "../desktopPlatform";
  import { theme, toggleTheme } from "../theme";
  import Search from "~icons/lucide/search";
  import SettingsIcon from "~icons/lucide/settings-2";
  import Sun from "~icons/lucide/sun";
  import Moon from "~icons/lucide/moon";

  // Title-bar integration differs per OS (ADR 0006): macOS reserves space for
  // the native Overlay traffic lights on the left; Windows is frameless and
  // draws its own caption controls on the right.
  const platform = currentDesktopPlatform();

  // Collapse props are retained (the layout drives rail collapse through them)
  // even though this bar no longer renders the toggle buttons.
  // The rail-collapse props are accepted (the layout passes them) but no longer
  // consumed here — the bar dropped its toggle buttons. Kept on the prop type so
  // the layout's call site stays unchanged and the buttons can return later.
  let {
    rightCollapsed: _rightCollapsed = false,
    onToggleLeft: _onToggleLeft,
    onToggleRight: _onToggleRight,
  }: {
    rightCollapsed?: boolean;
    onToggleLeft: () => void;
    onToggleRight: () => void;
  } = $props();

  const commandPaletteKeys = shortcutKeys(platform, "K");
  const settingsShortcut = shortcutLabel(platform, ",");

  // Daemon Status indicator (ADR 0009). A standalone quiet instrument: dot +
  // "daemon" + a status word — never color alone (design principle #3) — with a
  // click-to-open letterpress popover carrying the failure reason plus
  // View log / Restart. The four daemon states map to one word + dot class:
  //   running     → "connected"   (--st-ok dot w/ glow, word --ink-1 / 600)
  //   starting    → "starting"    (plain --ink-3 dot)
  //   unreachable → "unreachable" (--st-need dot)
  //   failed      → "failed"      (--st-need dot)
  const STATUS_LABEL = {
    starting: "starting",
    running: "connected",
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
  class="topbar"
  class:mac={platform === "macos"}
  class:win={platform === "windows"}
  data-tauri-drag-region
>
  <!-- Center: command palette trigger, absolutely centered to the window. -->
  <button class="palette-trigger no-drag" onclick={() => commandOpen.set(true)} aria-label="Open command palette">
    <Search class="icon seek" />
    <span class="ph">Jump to worktree, session, or action…</span>
    <span class="keys">
      {#each commandPaletteKeys as k (k)}<kbd>{k}</kbd>{/each}
    </span>
  </button>

  <!-- Right cluster: flex spacer pushes it right; theme toggle then daemon. -->
  <div class="grow" data-tauri-drag-region></div>

  <div class="right no-drag">
    <button
      class="bar-btn"
      title="Settings ({settingsShortcut})"
      aria-label="Open settings"
      onclick={() => void goto("/settings")}
    >
      <SettingsIcon class="icon" />
    </button>

    <button
      class="bar-btn"
      title={$theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
      aria-label={$theme === "dark" ? "Switch to light theme" : "Switch to dark theme"}
      aria-pressed={$theme === "dark"}
      onclick={() => toggleTheme()}
    >
      {#if $theme === "dark"}
        <Moon class="icon" />
      {:else}
        <Sun class="icon" />
      {/if}
    </button>

    <div class="daemon-wrap">
      <button
        class="daemon {$daemonStatus}"
        title="Daemon status"
        aria-haspopup="dialog"
        aria-expanded={statusOpen}
        onclick={() => (statusOpen = !statusOpen)}
      >
        <span class="dot"></span>
        <span class="word">daemon</span>
        <span class="status">{statusLabel}</span>
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
  </div>
</nav>

<style>
  .topbar {
    position: relative;
    display: flex;
    align-items: center;
    height: 100%;
    padding: 0 16px;
    background: linear-gradient(var(--paper-2), var(--paper-1));
    border-bottom: 1px solid var(--line);
    user-select: none;
  }
  /* macOS traffic lights are inset in tauri.conf.json so their centers sit on
     the top-bar row; this reserves their horizontal room. */
  .topbar.mac {
    padding-left: 78px;
  }
  /* Windows is frameless: the caption controls (WindowControls) are a fixed
     top-right layer; reserve their width (3 × 46px) so the bar's content never
     slides under them. */
  .topbar.win {
    padding-right: 138px;
  }

  /* Interactive children opt out of the drag region. */
  .no-drag {
    -webkit-app-region: no-drag;
  }

  .grow {
    flex: 1;
    align-self: stretch;
  }

  /* ---- Command palette trigger: centered to the window width, not the flex
     remainder (components.md ".palette"). ---- */
  .palette-trigger {
    position: absolute;
    left: 50%;
    top: 50%;
    translate: -50% -50%;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    width: 300px;
    max-width: 42vw;
    height: 26px;
    padding: 0 8px 0 9px;
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: 0;
    color: var(--ink-3);
    font: inherit;
    text-align: left;
    cursor: text;
    transition: border-color 0.18s ease-out;
  }
  .palette-trigger:hover {
    border-color: var(--ink-3);
  }
  .palette-trigger :global(.seek) {
    width: 13px;
    height: 13px;
    color: var(--ink-3);
    flex: none;
  }
  .palette-trigger .ph {
    flex: 1;
    min-width: 0;
    font-size: var(--r0);
    color: var(--ink-3);
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
  }
  .palette-trigger .keys {
    flex: none;
  }

  /* ---- Right cluster ---- */
  .right {
    display: flex;
    align-items: center;
    gap: 4px;
  }

  /* Shared 28px square bar button (settings, theme toggle). */
  .bar-btn {
    width: 28px;
    height: 28px;
    display: grid;
    place-items: center;
    border: 1px solid var(--line);
    border-radius: 0;
    background: var(--paper-2);
    color: var(--ink-2);
    cursor: pointer;
    transition:
      color 0.18s ease-out,
      border-color 0.18s ease-out,
      background 0.18s ease-out;
  }
  .bar-btn:hover {
    color: var(--ink-1);
    border-color: var(--ink-3);
  }
  .bar-btn :global(svg) {
    width: 15px;
    height: 15px;
  }
  .bar-btn:focus-visible {
    outline: 2px solid var(--iris);
    outline-offset: 1px;
  }

  /* ---- Daemon indicator: standalone, no shared border/background ---- */
  .daemon-wrap {
    position: relative;
    display: flex;
    align-items: center;
  }
  .daemon {
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 0 4px;
    background: transparent;
    border: 0;
    font-family: var(--mono);
    font-size: var(--r0);
    color: var(--ink-2);
    cursor: pointer;
  }
  .daemon:focus-visible {
    outline: 2px solid var(--iris);
    outline-offset: 1px;
  }
  .daemon .word {
    color: var(--ink-2);
  }
  .daemon .status {
    color: var(--ink-2);
  }
  /* Running ("connected"): emphasized word + ok dot with soft glow ring. */
  .daemon.running .status {
    color: var(--ink-1);
    font-weight: 600;
  }

  .dot {
    width: 7px;
    height: 7px;
    border-radius: 50%;
    background: var(--ink-3);
    flex: none;
  }
  .daemon.running .dot,
  .dot.running {
    background: var(--st-ok);
    box-shadow: 0 0 0 3px var(--st-ok-glow);
  }
  /* Non-running attention states share the oxide need color. */
  .daemon.unreachable .dot,
  .daemon.failed .dot,
  .dot.unreachable,
  .dot.failed {
    background: var(--st-need);
  }
  .dot.starting {
    background: var(--ink-3);
  }

  /* ---- Status popover: letterpress (paper-2, hairline, radius 0, crisp
     act-menu shadow via --shadow-pop). ---- */
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
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: 0;
    box-shadow: var(--shadow-pop);
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
    font-size: var(--r1);
    font-weight: 600;
    color: var(--ink-0);
    text-transform: capitalize;
  }
  .sp-reason {
    font-size: var(--r0);
    line-height: 1.5;
    color: var(--ink-2);
    font-family: var(--mono);
    white-space: pre-wrap;
    word-break: break-word;
    max-height: 160px;
    overflow-y: auto;
    margin: 0;
  }
  .sp-reason.ok {
    font-family: var(--ui);
    color: var(--ink-2);
  }
  .sp-actions {
    display: flex;
    gap: 6px;
  }
  .sp-btn {
    flex: 1;
    font: inherit;
    font-size: var(--r0);
    padding: 5px 8px;
    border-radius: 0;
    border: 1px solid var(--line);
    background: var(--paper-2);
    color: var(--ink-1);
    cursor: pointer;
    transition:
      background 0.18s ease-out,
      color 0.18s ease-out,
      border-color 0.18s ease-out;
  }
  .sp-btn:hover {
    background: var(--paper-3);
    border-color: var(--ink-3);
    color: var(--ink-0);
  }
  .sp-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
</style>
