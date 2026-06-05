<script lang="ts">
  // Windows caption controls (minimize · maximize/restore · close), drawn into
  // the right edge of the unified top nav. The window is frameless on Windows
  // (ADR 0006); these full-height, square buttons use native Win11 metrics
  // (46px wide, red close-hover) but the app's dark theme tokens so the bar
  // reads as native while matching the UI.
  //
  // The maximize button is special: a transparent native overlay window
  // (src-tauri/src/window_chrome.rs) is parked over its rectangle and hit-tests
  // as the real caption max button, so Windows 11 shows its Snap Layouts flyout
  // on hover. Because that overlay swallows the pointer, the webview gets no
  // `:hover` or click there — hover and click arrive as native bridge events
  // (hitch-max-button-hover / hitch-max-button-click). We still wire an
  // `onclick` as a fallback for the brief window before the overlay's rectangle
  // has been reported. We report that rectangle so the overlay can track the
  // button, but only AFTER those listeners are live (see onMount).
  import { onMount } from "svelte";
  import {
    closeWindow,
    minimizeWindow,
    onMaxButtonClick,
    onMaxButtonHover,
    reportMaxButtonRect,
    toggleMaximizeWindow,
    watchMaximized,
  } from "../windowChrome";

  let maximized = $state(false);
  let maxHovered = $state(false);
  let maxButton: HTMLButtonElement | undefined;

  onMount(() => {
    let disposed = false;
    let observer: ResizeObserver | undefined;

    // Keep the native overlay aligned with the button's real position across
    // layout shifts, window resizes, and DPI/monitor changes.
    const report = () => {
      if (maxButton) void reportMaxButtonRect(maxButton);
    };

    // watchMaximized only swaps the glyph; it doesn't park the overlay, so its
    // async initial sync can't drop a click.
    const stopMax = watchMaximized((m) => (maximized = m));

    // The native hover/click subscriptions register asynchronously. We must
    // have them live BEFORE reporting the button rect: reporting parks the
    // transparent overlay over the button, after which the webview no longer
    // sees DOM hover/clicks there. The native side emits hover/click directly
    // (Tauri events are not buffered for late subscribers), so parking before
    // the listeners are live would silently drop the first maximize click or
    // hover. Wait for both registrations (`ready`), then park.
    const hover = onMaxButtonHover((h) => (maxHovered = h));
    const click = onMaxButtonClick(() => void toggleMaximizeWindow());
    void Promise.all([hover.ready, click.ready]).then(() => {
      if (disposed) return;
      report();
      observer = new ResizeObserver(report);
      if (maxButton) observer.observe(maxButton);
    });
    window.addEventListener("resize", report);

    return () => {
      disposed = true;
      stopMax();
      hover.off();
      click.off();
      observer?.disconnect();
      window.removeEventListener("resize", report);
    };
  });
</script>

<div class="caption">
  <button
    class="cap min"
    title="Minimize"
    aria-label="Minimize"
    onclick={() => void minimizeWindow()}
  >
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0 5 H10" /></svg>
  </button>

  <button
    bind:this={maxButton}
    class="cap max"
    class:hovered={maxHovered}
    title={maximized ? "Restore" : "Maximize"}
    aria-label={maximized ? "Restore" : "Maximize"}
    onclick={() => void toggleMaximizeWindow()}
  >
    {#if maximized}
      <svg viewBox="0 0 10 10" aria-hidden="true">
        <path d="M2.5 2.5 H7.5 V7.5 H2.5 Z" />
        <path d="M2.5 0.5 H9.5 V7.5" />
      </svg>
    {:else}
      <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5 H9.5 V9.5 H0.5 Z" /></svg>
    {/if}
  </button>

  <button
    class="cap close"
    title="Close"
    aria-label="Close"
    onclick={() => void closeWindow()}
  >
    <svg viewBox="0 0 10 10" aria-hidden="true"><path d="M0.5 0.5 L9.5 9.5 M9.5 0.5 L0.5 9.5" /></svg>
  </button>
</div>

<style>
  /* Flush to the top-right corner of the window. Fixed (not in the nav flow) so
     the controls stay visible on full-window routes that hide the 3-pane shell
     — e.g. /settings — where the nav itself is display:none. The nav reserves
     matching right padding on Windows so its content never slides under here.
     z-index sits above route content; the native Snap-Layouts overlay tracks
     the max button via getBoundingClientRect, so fixed positioning is fine. */
  .caption {
    position: fixed;
    top: 0;
    right: 0;
    z-index: 50;
    height: 44px;
    display: flex;
  }
  .cap {
    width: 46px;
    align-self: stretch;
    display: grid;
    place-items: center;
    border: 0;
    background: transparent;
    color: var(--ink-1);
    cursor: default;
    padding: 0;
    transition: background 0.18s ease-out, color 0.18s ease-out;
  }
  .cap svg {
    width: 10px;
    height: 10px;
    fill: none;
    stroke: currentColor;
    stroke-width: 1;
    shape-rendering: geometricPrecision;
  }
  /* Hover fill. `.max.hovered` is driven by the native side (the button region
     is non-client, so CSS :hover can't fire there); min/close use real :hover. */
  .cap.min:hover,
  .cap.max.hovered {
    background: var(--paper-3);
    color: var(--ink-0);
  }
  .cap.close:hover {
    background: var(--st-need);
    color: var(--harness-fg);
  }
  .cap:focus-visible {
    outline: 2px solid var(--iris);
    outline-offset: -2px;
  }
</style>
