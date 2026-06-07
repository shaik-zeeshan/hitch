<script lang="ts">
  // Expand-all / collapse-all toggle for the section-list reading surfaces
  // (DiffAllTab and CommitTab heads), shared so the two stay identical. Sections
  // start collapsed by default; this is the one-click way out. Tri-state rule:
  // any section expanded → the action is "collapse all", otherwise "expand all".
  // Pure presentation — the caller owns the expanded state and the toggle effect
  // (DiffAllTab drives the daemon store + fan-out, CommitTab its local set).
  let { anyExpanded, onToggle }: { anyExpanded: boolean; onToggle: () => void } = $props();
</script>

<button
  type="button"
  class="xa-btn"
  title={anyExpanded ? "Collapse all sections" : "Expand all sections"}
  onclick={onToggle}
>{anyExpanded ? "collapse all" : "expand all"}</button>

<style>
  /* Same flat-rectangle language as DiffViewOptions' .dv-btn (mono label, dim by
     default, square border) — but an action button, so it has no pressed state. */
  .xa-btn {
    font-family: var(--mono);
    font-size: 0.6875rem;
    line-height: 1;
    color: var(--term-dim);
    background: transparent;
    border: 1px solid var(--term-line);
    border-radius: 0;
    padding: 3px 6px;
    cursor: pointer;
    white-space: nowrap;
    transition:
      color 0.12s ease-out,
      border-color 0.12s ease-out;
  }
  .xa-btn:hover {
    color: var(--term-fg);
    border-color: var(--term-dim);
  }
  .xa-btn:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: 1px;
  }
</style>
