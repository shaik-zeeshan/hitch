<script lang="ts">
  // Live PTY terminal (Paper Terminal .terminal panel). Ported from the React
  // TerminalPane: one
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
  import { theme as themeStore } from "../theme";
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
  import { dropTargetSession } from "../fileDrop";

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

  // Resolve the Paper Terminal palette for xterm. xterm's parser (esp. the WebGL
  // renderer) doesn't speak OKLCH or CSS var(), so we resolve everything to the
  // browser's USED rgb() string via a detached probe: setting an oklch/var()
  // color on an element and reading getComputedStyle(el).color back returns the
  // normalized rgb() value WebKit actually computed.
  //
  // Two layers (doc-design/colors.md):
  //  - SURFACE tokens (--term-bg2/--term-fg/--term-dim) differ per theme, so we
  //    resolve the live CSS var values — they track the active theme exactly.
  //    Background is the FLAT --term-bg2 (the active-tab/gradient top); the
  //    panel's CSS gradient shows in the host padding, while xterm's own grid
  //    sits on the flat fill so the WebGL renderer stays crisp.
  //  - In-terminal ANSI accents are LITERAL oklch values from colors.md
  //    ("In-terminal ANSI-ish accents"), one set per theme: the terminal
  //    surface follows the theme, so light gets darker accent inks tuned for
  //    the paper surface and dusk gets the brighter set tuned for deep ink.
  function resolveTheme(): ITheme {
    const probe = document.createElement("span");
    probe.style.display = "none";
    document.body.appendChild(probe);
    const rgb = (color: string): string => {
      probe.style.color = "";
      probe.style.color = color;
      return getComputedStyle(probe).color || "#d4d6da";
    };
    // colors.md in-terminal accents (literal; per theme).
    const dark =
      document.documentElement.getAttribute("data-theme") === "dark";
    const a = dark
      ? {
          grn: "oklch(82% 0.13 150)",
          red: "oklch(78% 0.13 28)",
          cy: "oklch(82% 0.10 195)",
          yl: "oklch(86% 0.12 92)",
          iris: "oklch(80% 0.10 280)",
          b: "oklch(96% 0.01 90)",
          // "black" must stay visible on the dark surface: the seam hairline.
          blk: "var(--term-line)",
          cursor: "oklch(82% 0.10 92)",
          sel: "oklch(60% 0.10 280 / 0.32)",
        }
      : {
          grn: "oklch(52% 0.13 150)",
          red: "oklch(52% 0.16 28)",
          cy: "oklch(52% 0.10 195)",
          yl: "oklch(58% 0.11 92)",
          iris: "oklch(48% 0.13 280)",
          // "white" must stay visible on the paper surface: a mid grey ink.
          b: "oklch(45% 0.012 90)",
          blk: "oklch(30% 0.02 265)",
          cursor: "oklch(48% 0.11 92)",
          sel: "oklch(50% 0.12 280 / 0.22)",
        };
    const theme: ITheme = {
      background: rgb("var(--term-bg2)"),
      foreground: rgb("var(--term-fg)"),
      cursor: rgb(a.cursor),
      cursorAccent: rgb("var(--term-bg2)"),
      selectionBackground: rgb(a.sel),
      black: rgb(a.blk),
      red: rgb(a.red),
      green: rgb(a.grn),
      yellow: rgb(a.yl),
      blue: rgb(a.iris),
      magenta: rgb(a.iris),
      cyan: rgb(a.cy),
      white: rgb(a.b),
      brightBlack: rgb("var(--term-dim)"),
      brightRed: rgb(a.red),
      brightGreen: rgb(a.grn),
      brightYellow: rgb(a.yl),
      brightBlue: rgb(a.iris),
      brightMagenta: rgb(a.iris),
      brightCyan: rgb(a.cy),
      brightWhite: rgb(a.b),
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

  // Re-resolve and reapply the xterm palette whenever the theme flips. The
  // surface tokens (--term-bg2/--term-fg/--term-dim) differ per theme, so a
  // light↔dark swap must repaint every mounted terminal — including hidden ones,
  // so they're already correct when re-shown. Reading $themeStore here registers
  // the dependency. No first-run gating: the effect's first run happens before
  // onMount creates `term` (so it bails on !term), and `term` isn't reactive —
  // gating on "first real change" here would swallow the first toggle instead.
  // Reapplying is idempotent and cheap, so every store change just repaints.
  $effect(() => {
    void $themeStore;
    if (!term) return;
    term.options.theme = resolveTheme();
  });

  onMount(() => {
    term = new Xterm({
      // Match the shell's --mono stack (JetBrains Mono) and the panel's
      // 0.8125rem / 13px body type from doc-design/components.md.
      fontFamily: '"JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace',
      fontSize: 13,
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

    // In-terminal search (Cmd+F on macOS, Ctrl+Shift+F elsewhere).
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

    // Platform terminal shortcuts. macOS uses Cmd/meta; Windows/Linux use
    // Ctrl+Shift for copy/search.
    // Returning false consumes the event; true lets xterm/the child handle it as
    // usual. The intent classification lives in the pure `classifyTerminalKey`
    // (unit-tested) so the routing — crucially that paste shortcuts are NOT
    // special and pass through to xterm's native paste (the SOLE keyboard paste
    // route) — is verifiable without a live DOM.
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
          // Copy only when there's a selection; otherwise let the child receive it.
          if (term?.hasSelection()) {
            void navigator.clipboard.writeText(term.getSelection());
            return false;
          }
          return true;
        case "search":
          void openSearch();
          return false;
        case "pass":
          // Includes paste shortcuts: native xterm handles paste on its textarea,
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

<!-- .terminal is the edge-to-edge panel (gradient ink, meets the column
     dividers + window bottom) and carries the panel inset (14px 16px 4px);
     .term-body is a clean UNPADDED host. FitAddon measures the host via
     getComputedStyle height/width, which WKWebView resolves to the PADDED
     border-box — padding on the host makes fit() over-count by ~a row/col and
     the grid's bottom row clips past the panel. Keep all inset on the wrapper
     so fit reads the true content area. -->
<div class="terminal">
  <!-- data-session-id lets the app-wide file-drop listener hit-test a drop
       point back to this session (see fileDrop.ts); the drop-target ring shows
       where dragged paths will land before release. -->
  <div
    class="term-body"
    class:drop-target={$dropTargetSession === sessionId}
    data-session-id={sessionId}
    bind:this={host}
  ></div>
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
  /* Edge-to-edge terminal panel: zero gutter, meets the column dividers and the
     window bottom directly. The panel carries the inset; its fill is the SAME
     flat --term-bg2 the xterm grid sits on, so the padding reads as part of
     the terminal (a gradient here visibly seams against the flat grid,
     especially on the light surface). */
  .terminal {
    position: relative;
    flex: 1;
    min-height: 0;
    height: 100%;
    width: 100%;
    display: flex;
    flex-direction: column;
    background: var(--term-bg2);
    border: none;
    border-radius: 0;
    padding: 14px 16px 4px;
    overflow: hidden;
  }
  /* Clean unpadded host so fit.fit() reads an exact content size (see the
     template comment — host padding skews FitAddon's measurement). */
  .term-body {
    flex: 1;
    min-height: 0;
    width: 100%;
  }
  /* Compact search overlay pinned to the top-right of the terminal. */
  .term-search {
    position: absolute;
    top: 10px;
    right: 14px;
    z-index: 5;
    display: flex;
    align-items: center;
    background: var(--term-bg2);
    border: 1px solid var(--term-line);
    border-radius: 0;
    padding: 3px 6px;
    box-shadow: var(--shadow-pop);
  }
  .term-search input {
    width: 180px;
    border: none;
    outline: none;
    background: transparent;
    color: var(--term-fg);
    font-family: var(--mono);
    font-size: var(--r1);
    line-height: 1.4;
  }
  .term-search input::placeholder {
    color: var(--term-dim);
  }
  .term-search:focus-within {
    border-color: var(--iris-ink);
  }
  /* "New output ↓" nudge: compact, on-theme, pinned bottom-right above the
     terminal. Sits inside the panel so it never overlaps the scrollbar edge. */
  .term-new-output {
    position: absolute;
    bottom: 12px;
    right: 16px;
    z-index: 5;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    background: var(--term-bg2);
    border: 1px solid var(--iris-ink);
    border-radius: 0;
    padding: 4px 10px;
    color: var(--term-fg);
    font-family: var(--mono);
    font-size: 0.6875rem;
    line-height: 1.2;
    cursor: pointer;
    box-shadow: var(--shadow-pop);
    transition: color 0.15s ease-out;
  }
  .term-new-output:hover {
    color: var(--iris-ink);
  }
  /* Drop-target affordance: an inset iris ring while an OS file drag hovers this
     terminal. inset box-shadow stays inside the host and doesn't shift xterm's
     layout (no reflow/fit). */
  .term-body.drop-target {
    box-shadow: inset 0 0 0 2px var(--iris-ink);
  }
  /* xterm injects its own canvas/layout; keep its viewport on theme. */
  .term-body :global(.xterm) {
    height: 100%;
    width: 100%;
  }
  .term-body :global(.xterm-viewport) {
    background: transparent !important;
  }
</style>
