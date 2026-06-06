<script lang="ts">
  // Compact diff view-options bar, shared by the single-file (DiffTab) and
  // all-changes (DiffAllTab) headers. Four controls, all flat Paper-Terminal
  // rectangles with a pressed state and a title tooltip:
  //   - Split / Unified layout (render-side, @pierre/diffs diffStyle)
  //   - Wrap long lines (render-side, overflow)
  //   - Ignore whitespace (re-diff — re-fetches the diff text)
  //   - Context lines stepper (re-diff — re-fetches), cycling 3 → 5 → 10 → 25
  // The stores themselves drive the diff renderer + daemon re-fetch (wired in
  // DiffTab / DiffAllTab / daemon.ts); this component only flips them.
  import {
    DEFAULT_DIFF_CONTEXT_LINES,
    diffContextLines,
    diffIgnoreWhitespace,
    diffStyle,
    diffWrap,
  } from "../settings";

  // The context-lines values the stepper cycles through. The default (3) is in
  // the set so the control always lands on a known rung.
  const CONTEXT_STEPS = [3, 5, 10, 25];

  function cycleContext() {
    const current = $diffContextLines;
    const index = CONTEXT_STEPS.indexOf(current);
    const next = CONTEXT_STEPS[(index + 1) % CONTEXT_STEPS.length] ?? DEFAULT_DIFF_CONTEXT_LINES;
    diffContextLines.set(next);
  }

  function toggleStyle() {
    diffStyle.set($diffStyle === "split" ? "unified" : "split");
  }
</script>

<div class="dv-opts" role="group" aria-label="Diff view options">
  <button
    type="button"
    class="dv-btn"
    class:on={$diffStyle === "split"}
    aria-pressed={$diffStyle === "split"}
    title={$diffStyle === "split" ? "Switch to unified view" : "Switch to split view"}
    onclick={toggleStyle}
  >split</button>
  <button
    type="button"
    class="dv-btn"
    class:on={$diffWrap}
    aria-pressed={$diffWrap}
    title={$diffWrap ? "Disable line wrap" : "Wrap long lines"}
    onclick={() => diffWrap.update((v) => !v)}
  >wrap</button>
  <button
    type="button"
    class="dv-btn"
    class:on={$diffIgnoreWhitespace}
    aria-pressed={$diffIgnoreWhitespace}
    title={$diffIgnoreWhitespace ? "Show whitespace changes" : "Ignore whitespace changes"}
    onclick={() => diffIgnoreWhitespace.update((v) => !v)}
  >ws</button>
  <button
    type="button"
    class="dv-btn dv-ctx"
    class:on={$diffContextLines !== DEFAULT_DIFF_CONTEXT_LINES}
    title="Context lines: {$diffContextLines} (click to cycle)"
    aria-label="Context lines, currently {$diffContextLines}"
    onclick={cycleContext}
  >ctx&nbsp;{$diffContextLines}</button>
</div>

<style>
  /* Flat rectangle language: square buttons, monospace labels, dim by default,
     iris-ink border + fg when pressed. Matches the header chrome (--term-*) so
     it reads as part of the diff head, not a floating widget. */
  .dv-opts {
    display: flex;
    align-items: center;
    gap: 4px;
    flex: none;
  }
  .dv-btn {
    font-family: var(--mono);
    font-size: 0.6875rem;
    line-height: 1;
    color: var(--term-dim);
    background: transparent;
    border: 1px solid var(--term-line);
    border-radius: 0;
    padding: 3px 6px;
    cursor: pointer;
    transition:
      color 0.12s ease-out,
      border-color 0.12s ease-out,
      background 0.12s ease-out;
  }
  .dv-btn:hover {
    color: var(--term-fg);
    border-color: var(--term-dim);
  }
  .dv-btn:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: 1px;
  }
  .dv-btn.on {
    color: var(--iris-ink);
    border-color: var(--iris-ink);
    background: oklch(from var(--iris-ink) l c h / 0.08);
  }
  .dv-ctx {
    font-variant-numeric: tabular-nums;
    white-space: nowrap;
  }
</style>
