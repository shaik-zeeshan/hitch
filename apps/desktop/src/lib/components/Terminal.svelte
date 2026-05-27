<script lang="ts">
  // Live PTY terminal (mockup .term). Ported from the React TerminalPane: one
  // xterm instance per session, fed from the daemon's per-session output buffer.
  // The component is re-keyed on session.id by the parent, so mount/destroy is
  // the full lifecycle — no need to react to a changing session prop here.
  //
  // Output diffing: `buffers[id]` is the whole accumulated stream. We track how
  // much we've written; on growth we append the tail, and on shrink (a reset on
  // reconnect) we wipe and repaint. Keystrokes go straight to the daemon; a
  // ResizeObserver fits the grid and reports the new cols/rows.
  import { onDestroy, onMount } from "svelte";
  import { Terminal as Xterm, type ITheme } from "@xterm/xterm";
  import { FitAddon } from "@xterm/addon-fit";
  import "@xterm/xterm/css/xterm.css";
  import { buffers, resizeSession, sendInput } from "../daemon";
  import type { Session } from "../types";

  let { session }: { session: Session } = $props();

  // Capture the id once: the parent re-keys this component per session, so the
  // id is stable for its lifetime. Reading `session.id` live inside the async
  // callbacks below is unsafe — when the active session closes, Svelte sets the
  // `session` prop to null (the parent's `{#if $activeSession}` value changes)
  // *before* this component unmounts. A live `session.id` read from the still-
  // attached `buffers` subscription would then throw, and because svelte/store
  // shares one module-level subscriber queue, a throw inside `set()` poisons it
  // and silently kills reactivity app-wide. Using the captured id avoids that.
  // svelte-ignore state_referenced_locally -- capturing the initial id is intended
  const sessionId = session.id;

  let host: HTMLDivElement;
  let term: Xterm | null = null;
  let fit: FitAddon | null = null;
  let written = 0;
  let resizeObserver: ResizeObserver | null = null;
  let unsubBuffer: (() => void) | null = null;

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

  function paint(buffer: string) {
    if (!term) return;
    if (buffer.length < written) {
      // Stream was reset (e.g. reattach after reconnect): wipe and repaint.
      term.reset();
      term.write(buffer);
    } else if (buffer.length > written) {
      term.write(buffer.slice(written));
    }
    written = buffer.length;
  }

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
    term.open(host);
    fit.fit();
    term.focus();

    term.onData((data) => sendInput(sessionId, data));

    // Subscribe fires immediately, painting whatever buffer already exists.
    unsubBuffer = buffers.subscribe(($buffers) => paint($buffers[sessionId] ?? ""));

    resizeObserver = new ResizeObserver(() => {
      if (!term || !fit) return;
      try {
        fit.fit();
      } catch {
        // Element may be momentarily detached during tab swaps.
      }
      void resizeSession(sessionId, term.cols, term.rows);
    });
    resizeObserver.observe(host);

    void resizeSession(sessionId, term.cols, term.rows);
  });

  onDestroy(() => {
    unsubBuffer?.();
    resizeObserver?.disconnect();
    term?.dispose();
    term = null;
    fit = null;
  });
</script>

<!-- Outer .term provides the visual padding; the inner host is a clean
     unpadded box so FitAddon measures the true content area and the rows it
     computes leave the padding (incl. the bottom) intact. -->
<div class="term">
  <div class="term-host" bind:this={host}></div>
</div>

<style>
  .term {
    height: 100%;
    width: 100%;
    background: var(--bg-0);
    padding: 12px 14px;
    overflow: hidden;
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
