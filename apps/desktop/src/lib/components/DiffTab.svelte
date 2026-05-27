<script lang="ts">
  // Diff view (mockup .diff): a unified diff rendered as classified rows with a
  // line-number gutter. Reads the daemon-fetched diff text from the store and
  // classifies it locally (see lib/diff.ts) to match the locked design, which
  // shows a flat, un-highlighted diff. Peer of the session terminal in the
  // center; the tab + close affordance live in SessionTabs.
  import { diffPath, diffText } from "../daemon";
  import { parseDiff } from "../diff";

  const parsed = $derived($diffText === null ? null : parseDiff($diffText));
</script>

<div class="diff">
  <div class="diff-head">
    <svg width="14" height="14" viewBox="0 0 16 16" fill="none" stroke="var(--tx-lo)" stroke-width="1.3"
      ><rect x="2.5" y="2.5" width="11" height="11" rx="2" /><line x1="2.5" y1="6.5" x2="13.5" y2="6.5" /></svg
    >
    <span class="path">{$diffPath}</span>
    {#if parsed && !parsed.isEmpty}
      <span class="meta">
        <span class="add">+{parsed.additions}</span>
        <span class="del">−{parsed.deletions}</span>
      </span>
    {/if}
  </div>

  {#if parsed === null}
    <div class="diff-empty"><p>Loading diff…</p></div>
  {:else if parsed.isBinary}
    <div class="diff-empty"><p>Binary file — no text diff.</p></div>
  {:else if parsed.isEmpty}
    <div class="diff-empty"><p>No textual changes.</p></div>
  {:else}
    <pre>{#each parsed.lines as line, i (i)}<span class="dl {line.kind}"><span class="gut"
            >{line.gutter ?? ""}</span>{line.text}</span>{/each}</pre>
  {/if}
</div>

<style>
  /* Ported verbatim from hitch-shell-mockup.html .diff block. */
  .diff {
    height: 100%;
    width: 100%;
    background: var(--bg-0);
    overflow-y: auto;
  }
  .diff-head {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 14px;
    border-bottom: 1px solid var(--line);
    position: sticky;
    top: 0;
    background: var(--bg-0);
    z-index: 1;
  }
  .diff-head .path {
    font-family: var(--mono);
    font-size: 12px;
    color: var(--tx-hi);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .diff-head .meta {
    font-size: 11px;
    color: var(--tx-lo);
    margin-left: auto;
    font-family: var(--mono);
    flex: none;
  }
  .diff-head .add {
    color: var(--ok);
  }
  .diff-head .del {
    color: var(--err);
    margin-left: 4px;
  }
  .diff pre {
    font-family: var(--mono);
    font-size: 12px;
    line-height: 1.5;
    padding: 0;
    margin: 0;
  }
  .dl {
    display: block;
    padding: 0 14px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .dl .gut {
    display: inline-block;
    width: 30px;
    color: oklch(48% 0.01 265);
    user-select: none;
    text-align: right;
    padding-right: 12px;
  }
  .dl.hunk {
    color: var(--ac-bright);
    background: oklch(34% 0.06 265 / 0.25);
  }
  .dl.add {
    background: oklch(60% 0.12 150 / 0.1);
    color: oklch(86% 0.06 150);
  }
  .dl.del {
    background: oklch(58% 0.14 25 / 0.1);
    color: oklch(83% 0.08 25);
  }
  .dl.ctx {
    color: var(--tx-md);
  }

  .diff-empty {
    display: grid;
    place-content: center;
    height: calc(100% - 38px);
    padding: 24px;
  }
  .diff-empty p {
    font-size: 12px;
    color: var(--tx-lo);
  }
</style>
