<script lang="ts">
  // Live PTY terminal (mockup .term). Ported from the React TerminalPane: one
  // xterm instance per session, fed from the daemon's per-session binary output
  // channel (ADR 0007). Center.svelte keys a Terminal off EVERY session (across
  // all parents, not just the active one) and toggles visibility (the `active`
  // prop) rather than mounting/unmounting per tab/diff/worktree switch, so
  // scroll + buffer + the PTY-aligned grid survive every switch. This instance
  // lives for exactly one session for its whole lifetime: it mounts when the
  // session first appears and unmounts ONLY when the session closes (removed
  // from the keyed list) — NOT on a parent/worktree switch. There is therefore
  // no changing `session` prop to react to.
  //
  // Output flow: raw PTY bytes arrive as Uint8Array tails from
  // `subscribeSessionOutput`; we feed them straight to `term.write`, which does
  // its OWN streaming UTF-8 decode across frames (so multi-byte glyphs split
  // across a read boundary render correctly). We track `written` as a TOTAL-
  // BYTES-SEEN offset, not a buffer length, so the bounded byte ring's head
  // trims never corrupt the append math. On reset (a reconnect replay) we wipe
  // and let the ring repopulate. Keystrokes go straight to the daemon; a
  // ResizeObserver fits the grid and reports the new cols/rows.
  import { onDestroy, onMount, tick } from "svelte";
  import {
    Terminal as Xterm,
    type IDisposable,
    type ITheme,
  } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import { WebLinksAddon } from "@xterm/addon-web-links";
  import { SearchAddon } from "@xterm/addon-search";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import "@xterm/xterm/css/xterm.css";
  import {
    recordTerminalSize,
    repaintSession,
    resizeSessionDebounced,
    sendInput,
    subscribeSessionOutput,
  } from "../daemon";
  import { WebglAddon } from "@xterm/addon-webgl";
  import type { Session } from "../types";
  import { classifyTerminalKey } from "../terminalKeys";
  import { createOutputBatcher } from "../outputBatch";
  import { releaseWebgl, retainWebgl, touchWebgl } from "../webglBudget";

  // `active` is true only when this is the visible terminal (its tab is
  // selected AND the diff isn't covering the view). The parent keeps every
  // session's Terminal mounted and toggles `active`; we use its rising edge
  // (hidden→visible) to drive fit-on-activate.
  let { session, active = true }: { session: Session; active?: boolean } =
    $props();

  // Capture the id once: this Terminal instance owns exactly one session for
  // its whole lifetime (see the file comment), so the id is stable. Reading
  // `session.id` live inside the async callbacks below would still be the right
  // value, but capturing keeps the data path independent of any prop churn and
  // makes the closing-session teardown order irrelevant: when this session
  // closes it is removed from the parent's keyed list and this component
  // unmounts (onDestroy unsubscribes) — `session` is never reassigned to null
  // under us. (A throw inside an output `set()` would poison svelte/store's
  // shared subscriber queue and kill reactivity app-wide, so the captured id
  // also guards against any transient prop state.)
  // svelte-ignore state_referenced_locally -- capturing the initial id is intended
  const sessionId = session.id;

  let host: HTMLDivElement;
  let term: Xterm | null = null;
  let opened = false;
  let fit: FitAddon | null = null;
  let webLinks: WebLinksAddon | null = null;
  let searchAddon: SearchAddon | null = null;
  // GPU renderer, attached when this terminal first becomes active with a real
  // size. Each WebGL context counts against the browser's ~16 live-context cap,
  // so rather than churn a context on every hide we keep the most-recently-
  // active terminals WARM via the module-level webglBudget LRU: hiding this
  // terminal no longer disposes its context; the budget evicts the coldest
  // terminal only when the warm set overflows its cap. null means we're on
  // xterm's default DOM renderer (not yet attached, evicted by the budget, or
  // WebGL is unsupported / lost its context).
  let webgl: WebglAddon | null = null;
  // Total PTY bytes seen by this terminal (the offset basis; matches the ring's
  // `totalSeen`), not a buffer length — so head-trims in the byte ring never
  // corrupt the append math.
  let written = 0;
  let resizeObserver: ResizeObserver | null = null;
  let unsubOutput: (() => void) | null = null;
  // Pending rAF handle for the coalesced local fit (see `scheduleFit`).
  let fitFrame: number | null = null;

  // In-terminal search overlay state.
  let searchOpen = $state(false);
  let searchQuery = $state("");
  let searchInput: HTMLInputElement | null = $state(null);

  // "New output ↓" nudge. xterm v6 does NOT yank a scrolled-up viewport to the
  // bottom on write (the buffer grows — baseY rises — while viewportY stays put,
  // so the reader keeps their place). We detect that state and surface a small
  // pill so the user knows fresh output is waiting below. `showNewOutput` drives
  // the pill; `atBottom` is the live "is the viewport pinned to the bottom?"
  // flag, recomputed on every scroll.
  let showNewOutput = $state(false);
  let atBottom = true;
  let scrollDisposable: IDisposable | null = null;
  let writeDisposable: IDisposable | null = null;

  // Bottom = the viewport's top line is at (or past) the last page's top line.
  // Scrolled up means viewportY < baseY.
  function isAtBottom(): boolean {
    if (!term) return true;
    const buffer = term.buffer.active;
    return buffer.viewportY >= buffer.baseY;
  }

  // Click handler for the pill: jump to the freshest output and dismiss.
  function scrollToNewOutput() {
    term?.scrollToBottom();
    showNewOutput = false;
  }

  async function openSearch() {
    searchOpen = true;
    await tick();
    searchInput?.focus();
    searchInput?.select();
  }

  function closeSearch() {
    searchOpen = false;
    term?.focus();
  }

  // Right-click → paste. We OWN the context menu: suppressing the webview's
  // default menu also suppresses any native paste it would offer, so this is the
  // ONE remaining manual term.paste call and it cannot double-fire with another
  // path. Routed through term.paste so bracketed-paste (DECSET 2004) is honored.
  // (tauri.conf.json does not independently enable a context menu — suppression
  // lives entirely in this handler.) Guarded against a not-yet/already-disposed
  // term so a stray right-click during mount/teardown is a no-op.
  function onContextMenu(e: MouseEvent) {
    e.preventDefault();
    if (!term) return;
    void navigator.clipboard.readText().then((t) => term?.paste(t));
  }

  function onSearchKeydown(e: KeyboardEvent) {
    if (e.key === "Enter") {
      e.preventDefault();
      if (!searchQuery) return;
      if (e.shiftKey) searchAddon?.findPrevious(searchQuery);
      else searchAddon?.findNext(searchQuery);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearch();
    }
  }

  // Resolve the design tokens (OKLCH) to rgb() via the browser so xterm's
  // canvas parser — which doesn't speak OKLCH — gets values it can render, and
  // the terminal palette tracks the locked theme exactly.
  function resolveTheme(): ITheme {
    const probe = document.createElement("span");
    probe.style.display = "none";
    document.body.appendChild(probe);
    const rgb = (color: string): string => {
      probe.style.color = "";
      probe.style.color = color;
      return getComputedStyle(probe).color || "#d4d6da";
    };
    const theme: ITheme = {
      background: rgb("var(--bg-0)"),
      foreground: rgb("oklch(86% 0.008 265)"),
      cursor: rgb("var(--ac)"),
      cursorAccent: rgb("var(--bg-0)"),
      selectionBackground: rgb("oklch(50% 0.08 265 / 0.35)"),
      black: rgb("oklch(30% 0.008 265)"),
      red: rgb("var(--err)"),
      green: rgb("oklch(80% 0.14 150)"),
      yellow: rgb("var(--warn)"),
      blue: rgb("var(--ac-bright)"),
      magenta: rgb("oklch(78% 0.13 320)"),
      cyan: rgb("oklch(82% 0.10 200)"),
      white: rgb("var(--tx-md)"),
      brightBlack: rgb("var(--tx-lo)"),
      brightRed: rgb("oklch(74% 0.17 25)"),
      brightGreen: rgb("oklch(85% 0.14 150)"),
      brightYellow: rgb("oklch(87% 0.13 75)"),
      brightBlue: rgb("oklch(82% 0.13 265)"),
      brightMagenta: rgb("oklch(83% 0.13 320)"),
      brightCyan: rgb("oklch(87% 0.10 200)"),
      brightWhite: rgb("var(--tx-hi)"),
    };
    probe.remove();
    return theme;
  }

  // Coalesce PTY output to one xterm write per animation frame. The daemon
  // delivers bytes as many small tails per frame (one per channel message);
  // writing each tail separately thrashes xterm's parser under bursty output.
  // The batcher buffers tails arriving within a frame and hands xterm a SINGLE
  // concatenated write per frame. Concatenating the RAW bytes in arrival order
  // keeps multi-byte UTF-8 sequences split across tails intact, and xterm's
  // streaming decoder sees the exact same byte stream it would have per-tail.
  // The injected scheduler is real rAF here; tests drive a synchronous one.
  const outputBatch = createOutputBatcher({
    schedule: (flush) => requestAnimationFrame(flush),
    cancel: (handle) => cancelAnimationFrame(handle),
    emit: (chunk) => {
      if (!term) return;
      term.write(chunk);
      // Keep `written` as TOTAL-BYTES-SEEN: bump by exactly the bytes handed to
      // xterm, identical to the pre-batching per-tail accounting.
      written += chunk.length;
    },
  });

  // New PTY bytes for this session: buffer the tail; the batcher flushes one
  // concatenated `term.write` on the next animation frame (see above).
  function onData(tail: Uint8Array) {
    outputBatch.push(tail);
  }

  // The byte ring was reset (a reconnect scrollback replay): drop any buffered-
  // but-unflushed tails (they belong to the pre-reset stream and must not paint
  // after the wipe), reset the offset, and clear the terminal so the replayed
  // bytes repaint cleanly.
  function onReset() {
    outputBatch.reset();
    written = 0;
    term?.reset();
  }

  // Subscribe to PTY output, but ONLY once the terminal is open and fitted to
  // its real grid. subscribeSessionOutput replays the ring immediately (catch-
  // up), and xterm parses that replay against its CURRENT size — so subscribing
  // before open + fit parses it at xterm's default 80x24, and the later
  // fit-on-activate only reflows the already-mangled buffer instead of
  // re-parsing it at the right size (cursor-addressed TUI output and wrapped
  // lines land wrong). A hidden tab therefore defers this until activate(); the
  // session ring keeps accumulating live bytes meanwhile, so the deferred
  // catch-up loses nothing. Idempotent: the first caller wins.
  function ensureSubscribed() {
    if (unsubOutput) return;
    unsubOutput = subscribeSessionOutput(sessionId, { onReset, onData });
  }

  // True when the host has no measurable area — a hidden (`display:none`)
  // terminal or one momentarily detached during a parent/tab swap. xterm can't
  // be measured then, and fitting it corrupts the grid, so all sizing is a
  // no-op until the box has real dimensions.
  function isZeroSize(): boolean {
    return !host || host.clientWidth === 0 || host.clientHeight === 0;
  }

  // xterm's `open()` measures DOM at attach time and requires a visible,
  // measurable parent. Hidden session slots are `display:none`, so a Terminal
  // may mount before it can be safely opened. Create the xterm instance and
  // addons eagerly, but attach its DOM only when this slot is active and sized.
  function openIfMeasurable(): boolean {
    if (!term || isZeroSize()) return false;
    if (!opened) {
      term.open(host);
      opened = true;
    }
    return true;
  }

  // Reflow the local grid to the host size. Cheap and synchronous; the daemon
  // is NOT told here — that goes through the trailing debounce so the PTY child
  // sees one resize after the size settles, not one per frame.
  function fitLocal() {
    if (!term || !fit || !opened || isZeroSize()) return;
    try {
      fit.fit();
      // Remember the grid we settled on so the NEXT session's PTY can open at
      // this exact size (the common, flash-free case in daemon.openSession).
      recordTerminalSize(term.cols, term.rows);
    } catch {
      // Element may be momentarily detached during tab/parent swaps.
    }
  }

  // Coalesce fits to at most one per animation frame: a ResizeObserver storm
  // (window drag, panel toggle) collapses into a single fit per frame so the
  // local reflow stays smooth without thrashing layout.
  function scheduleFit() {
    if (fitFrame !== null) cancelAnimationFrame(fitFrame);
    fitFrame = requestAnimationFrame(() => {
      fitFrame = null;
      if (!active || !openIfMeasurable() || !term) return;
      fitLocal();
      // Now open + sized: safe to start (or catch up) the output replay. Covers
      // the rare active-but-zero-size-at-mount case, where neither the visible
      // mount path nor activate() ran but the box later gains a real size.
      ensureSubscribed();
      // Tell the daemon the (possibly) new size, but trailing-debounced so the
      // child gets ONE SIGWINCH after the drag settles.
      resizeSessionDebounced(sessionId, term.cols, term.rows);
    });
  }

  // Attach the WebGL renderer to the LIVE, visible terminal. Guarded so it is
  // never attached to a hidden / zero-size terminal (WebGL on a `display:none`
  // box wastes a scarce context and renders nothing) and never double-attached.
  // WebGL can fail (unsupported GPU/driver, headless) or lose its context at
  // runtime; on any such failure we silently fall back to xterm's default DOM
  // renderer — the terminal keeps working, just without GPU acceleration.
  // On success we register the context with the module-level warm-LRU budget so
  // it can later evict THIS terminal (calling disposeWebgl) when too many
  // contexts are live, instead of every hide churning its own context.
  function attachWebgl() {
    if (!term || webgl || isZeroSize()) return;
    try {
      const addon = new WebglAddon();
      // A lost context (GPU reset, tab backgrounded, driver hiccup) would leave
      // a dead canvas; dispose so xterm transparently reverts to DOM rendering.
      // The addon disposed itself here, so also drop it from the warm budget so
      // it doesn't hold a stale disposer pointing at a dead context.
      addon.onContextLoss(() => {
        disposeWebgl();
        releaseWebgl(sessionId);
      });
      term.loadAddon(addon);
      webgl = addon;
      // Now warm: keep this context alive across hides until the budget evicts
      // it. The disposer releases THIS terminal's actual addon on eviction.
      retainWebgl(sessionId, () => disposeWebgl());
    } catch {
      // WebGL unavailable — stay on the default renderer. Never re-throw.
      webgl = null;
    }
  }

  // Release the GPU context (on hide, context loss, or destroy). Guarded so it
  // can be called unconditionally; xterm reverts to the DOM renderer.
  function disposeWebgl() {
    try {
      webgl?.dispose();
    } catch {
      // Already disposed with the terminal, or never fully attached.
    }
    webgl = null;
  }

  // Catch a hidden terminal up when it becomes visible: while hidden its
  // ResizeObserver ticks are no-ops (zero-size guard), so its grid can be stale
  // relative to the current pane size. After layout has applied the visibility
  // change (next frame), fit immediately, notify the daemon of the fresh size
  // (through the per-session debounce, so this coalesces with any concurrent
  // observer tick into one resize), and focus.
  function activate() {
    requestAnimationFrame(() => {
      if (!active || !openIfMeasurable() || !term) return;
      fitLocal();
      // Open + fitted to the live grid: now replay the ring at the correct size.
      ensureSubscribed();
      resizeSessionDebounced(sessionId, term.cols, term.rows);
      // Force a clean repaint even when the size did NOT change (returning to a
      // tab whose grid is identical): the debounce may resize to the same size
      // or coalesce away, but this unconditional repaint still makes Claude Code
      // redraw a crisp frame. Best-effort; never throws (see daemon helper).
      void repaintSession(sessionId);
      // Now that the box has a real size, GPU-accelerate the active terminal.
      attachWebgl();
      term.focus();
    });
  }

  // Drive open + fit-on-activate off the rising edge of `active` (hidden→visible).
  // A visible initial mount opens/fits/focuses in onMount, so only react to changes.
  // `wasActive` seeds from the initial `active` purely to anchor the edge
  // detector; the $effect below keeps it in sync on every subsequent change.
  // svelte-ignore state_referenced_locally -- seeding the edge detector is intended
  let wasActive = active;
  $effect(() => {
    if (active && !wasActive) activate();
    // Falling edge (visible→hidden): KEEP the GPU context warm instead of
    // disposing it. The module-level webglBudget caps total live contexts well
    // under the browser's ~16 limit and evicts the least-recently-active when
    // the warm set overflows, so switching tabs/parents no longer churns a
    // context per hide. touchWebgl bumps recency so the terminal the user just
    // left stays at the warm end of the LRU (cheap to re-show).
    else if (!active && wasActive) touchWebgl(sessionId);
    wasActive = active;
  });

  onMount(() => {
    term = new Xterm({
      fontFamily:
        '"Berkeley Mono", ui-monospace, "SF Mono", "JetBrains Mono", Menlo, monospace',
      fontSize: 12.5,
      theme: resolveTheme(),
      cursorBlink: true,
      scrollback: 5000,
    });
    fit = new FitAddon();
    term.loadAddon(fit);

    // Clickable links: open in the user's real browser (Tauri opener), never
    // navigate the webview. The custom activate handler suppresses xterm's
    // default window.open behavior.
    webLinks = new WebLinksAddon((_event, uri) => {
      void openUrl(uri);
    });
    term.loadAddon(webLinks);

    // In-terminal search (Cmd+F overlay below).
    searchAddon = new SearchAddon();
    term.loadAddon(searchAddon);

    // "New output ↓" nudge wiring. On scroll, recompute whether the viewport is
    // pinned to the bottom; the moment the user returns to the bottom (by
    // scrolling, or via the pill), the nudge clears itself. On a parsed write,
    // if the reader is scrolled up the new output landed below their view, so
    // raise the pill.
    scrollDisposable = term.onScroll(() => {
      atBottom = isAtBottom();
      if (atBottom) showNewOutput = false;
    });
    writeDisposable = term.onWriteParsed(() => {
      atBottom = isAtBottom();
      if (!atBottom) showNewOutput = true;
    });

    // macOS Cmd shortcuts. Only intercept e.metaKey (Cmd) — never Ctrl, so
    // Ctrl+C stays SIGINT to the child. Returning false consumes the event;
    // true lets xterm/the child handle it as usual. The intent classification
    // lives in the pure `classifyTerminalKey` (unit-tested) so the routing —
    // crucially that Cmd+V is NOT special and passes through to xterm's native
    // paste (the SOLE keyboard paste route) — is verifiable without a live DOM.
    term.attachCustomKeyEventHandler((e) => {
      switch (classifyTerminalKey(e)) {
        case "newline":
          // Send Shift+Enter as a line feed (\n) rather than carriage return
          // (\r) so apps (e.g. Claude Code) can tell Enter (execute) apart from
          // Shift+Enter (insert newline). Consume so xterm doesn't also emit \r.
          // xterm.js 6.1 may handle this natively; remove the workaround then.
          sendInput(sessionId, "\n");
          return false;
        case "suppress":
          // Non-keydown Shift+Enter phases are consumed without sending another
          // byte, preserving the old behavior: one LF, no native Enter fallback.
          return false;
        case "copy":
          // Copy only when there's a selection; otherwise let Cmd+C through.
          if (term?.hasSelection()) {
            void navigator.clipboard.writeText(term.getSelection());
            return false;
          }
          return true;
        case "search":
          void openSearch();
          return false;
        case "pass":
          // Includes Cmd+V: native xterm handles paste on its textarea, which
          // already honors bracketed-paste (DECSET 2004). No manual term.paste
          // here, so the keyboard paste path cannot double-fire.
          return true;
      }
    });

    term.onData((data) => sendInput(sessionId, data));

    // Output subscription is deferred to the first open + fit (see
    // ensureSubscribed): the visible-mount path below for a tab that mounts
    // active, or activate()/scheduleFit() for one that mounts hidden — so the
    // ring's catch-up replay is always parsed against the real grid.

    // Every observed size change schedules a coalesced local fit (rAF) and a
    // trailing-debounced daemon notify. A hidden terminal still ticks here
    // (e.g. when the window resizes), but the active + zero-size guards inside
    // make those ticks no-ops — fit-on-activate catches the size up when shown.
    resizeObserver = new ResizeObserver(() => scheduleFit());
    resizeObserver.observe(host);

    // Own the right-click menu (paste). On `host` so it covers the whole
    // terminal area; removed in onDestroy.
    host.addEventListener("contextmenu", onContextMenu);

    // Open + fit + focus only if we mounted visible. A non-active tab in the
    // same parent mounts hidden (`display:none`) alongside the active one; even
    // xterm.open() must be deferred until activation so xterm's DOM measurement
    // happens against a real box.
    if (active && openIfMeasurable() && term) {
      fitLocal();
      // Open + fitted before the first replay, so the ring catch-up parses at
      // the real grid rather than xterm's default 80x24.
      ensureSubscribed();
      // Mounted visible — GPU-accelerate immediately (the rising-edge $effect
      // won't fire for an already-active mount).
      attachWebgl();
      term.focus();
      resizeSessionDebounced(sessionId, term.cols, term.rows);
    }
  });

  onDestroy(() => {
    unsubOutput?.();
    resizeObserver?.disconnect();
    host?.removeEventListener("contextmenu", onContextMenu);
    // Cancel any frame queued by scheduleFit so it can't run against a disposed
    // term. The per-session daemon debounce timer is cleared centrally in
    // daemon.ts on session close (closeSessionOutput); a parent-switch unmount
    // leaves the session alive, so letting that trailing timer fire is benign.
    if (fitFrame !== null) {
      cancelAnimationFrame(fitFrame);
      fitFrame = null;
    }
    // Cancel any pending output-batch flush so it can't write against a disposed
    // term. Buffered bytes are dropped — fine, the terminal is going away.
    outputBatch.reset();
    // Drop the scroll / write listeners feeding the "new output" nudge.
    scrollDisposable?.dispose();
    writeDisposable?.dispose();
    scrollDisposable = null;
    writeDisposable = null;
    // Dispose addons before the terminal. Guarded so unmount never throws.
    // WebGL first so its GPU context is released even if a later dispose throws.
    // Then drop this terminal from the warm budget AFTER disposing our own addon
    // (releaseWebgl does not call the disposer again — no double-dispose).
    disposeWebgl();
    releaseWebgl(sessionId);
    try {
      webLinks?.dispose();
    } catch {
      // Addon may already be torn down with the terminal.
    }
    try {
      searchAddon?.dispose();
    } catch {
      // Addon may already be torn down with the terminal.
    }
    term?.dispose();
    term = null;
    opened = false;
    fit = null;
    webLinks = null;
    searchAddon = null;
  });
</script>

<!-- Outer .term provides the visual padding; the inner host is a clean
     unpadded box so FitAddon measures the true content area and the rows it
     computes leave the padding (incl. the bottom) intact. -->
<div class="term">
  <div class="term-host" bind:this={host}></div>
  {#if searchOpen}
    <!-- Compact in-terminal search box: Enter → next, Shift+Enter → prev,
         Esc → close. Styled with the app's terminal tokens. -->
    <div class="term-search">
      <input
        bind:this={searchInput}
        bind:value={searchQuery}
        onkeydown={onSearchKeydown}
        type="text"
        placeholder="Search…"
        spellcheck="false"
        autocapitalize="off"
        autocorrect="off"
      />
    </div>
  {/if}
  {#if showNewOutput}
    <!-- Compact nudge pinned bottom-right: output arrived while the reader was
         scrolled up. Click jumps to the bottom and dismisses; returning to the
         bottom by scrolling also dismisses it (onScroll). -->
    <button
      class="term-new-output"
      type="button"
      onclick={scrollToNewOutput}
    >
      New output ↓
    </button>
  {/if}
</div>

<style>
  .term {
    position: relative;
    height: 100%;
    width: 100%;
    background: var(--bg-0);
    padding: 12px 14px;
    overflow: hidden;
  }
  /* Compact search overlay pinned to the top-right of the terminal. */
  .term-search {
    position: absolute;
    top: 8px;
    right: 12px;
    z-index: 5;
    display: flex;
    align-items: center;
    background: var(--bg-2);
    border: 1px solid var(--line);
    border-radius: 6px;
    padding: 3px 6px;
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.35);
  }
  .term-search input {
    width: 180px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--tx-hi);
    font-family:
      "Berkeley Mono", ui-monospace, "SF Mono", "JetBrains Mono", Menlo,
      monospace;
    font-size: 12px;
    line-height: 1.4;
  }
  .term-search input::placeholder {
    color: var(--tx-lo);
  }
  .term-search:focus-within {
    border-color: var(--ac);
  }
  /* "New output ↓" nudge: compact, on-theme, pinned bottom-right above the
     terminal. Sits inside the .term padding so it never overlaps the scrollbar
     edge. Accent-tinted so it reads as actionable without shouting. */
  .term-new-output {
    position: absolute;
    bottom: 12px;
    right: 16px;
    z-index: 5;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--bg-2);
    border: 1px solid var(--ac);
    border-radius: 999px;
    padding: 4px 10px;
    color: var(--tx-hi);
    font-family:
      "Berkeley Mono", ui-monospace, "SF Mono", "JetBrains Mono", Menlo,
      monospace;
    font-size: 11px;
    line-height: 1.2;
    cursor: pointer;
    box-shadow: 0 4px 14px rgb(0 0 0 / 0.35);
  }
  .term-new-output:hover {
    background: var(--bg-1);
    color: var(--ac);
  }
  /* Clean inner box (no padding) so fit.fit() reads an exact content size. */
  .term-host {
    height: 100%;
    width: 100%;
  }
  /* xterm injects its own canvas/layout; keep its viewport scrollbar on theme. */
  .term-host :global(.xterm) {
    height: 100%;
    width: 100%;
  }
  .term-host :global(.xterm-viewport) {
    background: transparent !important;
  }
</style>
