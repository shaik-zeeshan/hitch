<script lang="ts">
  // Agent quick-launch bar (mockup .agentbar). Claude / Codex spawn a session
  // running that agent's CLI in the active parent; `+` opens a plain shell.
  import { openSession } from "../daemon";
  import type { SessionParent } from "../types";

  let { parent }: { parent: SessionParent } = $props();
</script>

<div class="agentbar">
  <button class="launch claude" onclick={() => void openSession(parent, "claude", ["claude"])}>
    <span class="gm">✳</span> Claude
  </button>
  <button class="launch codex" onclick={() => void openSession(parent, "codex", ["codex"])}>
    <span class="gm">⬡</span> Codex
  </button>
  <button
    class="plus"
    title="New shell session"
    aria-label="New shell session"
    onclick={() => void openSession(parent, "shell", null)}>+</button
  >
</div>

<style>
  .agentbar {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 7px 10px;
    border-bottom: 1px solid var(--line-soft);
    min-width: 0;
  }
  .launch {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 5px 10px 5px 8px;
    border-radius: var(--radius);
    background: var(--bg-3);
    border: 1px solid var(--line);
    color: var(--tx-hi);
    font: inherit;
    font-size: 11.5px;
    cursor: pointer;
    flex: none;
    transition:
      background var(--t-fast),
      border-color var(--t-fast);
  }
  .launch:hover {
    background: var(--bg-4);
    border-color: oklch(38% 0.012 265);
  }
  .launch .gm {
    font-size: 13px;
    line-height: 1;
  }
  .launch.claude .gm {
    color: var(--warn);
  }
  .launch.codex .gm {
    color: var(--tx-md);
  }
  .plus {
    margin-left: 2px;
    width: 28px;
    height: 28px;
    border-radius: var(--radius);
    border: 1px solid var(--line);
    background: var(--bg-3);
    color: var(--tx-md);
    cursor: pointer;
    font-size: 16px;
    display: grid;
    place-items: center;
    flex: none;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .plus:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
</style>
