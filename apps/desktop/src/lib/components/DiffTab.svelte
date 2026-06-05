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
    <span class="glyph" aria-hidden="true">±</span>
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
  /* The diff is the center peer of the terminal: it sits on the same edge-to-
     edge deep-ink panel so closing it reveals the terminal seamlessly. */
  .diff {
    height: 100%;
    width: 100%;
    background: linear-gradient(var(--term-bg2), var(--term-bg));
    overflow-y: auto;
  }
  .diff-head {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 16px;
    border-bottom: 1px solid var(--term-line);
    position: sticky;
    top: 0;
    background: var(--term-bg2);
    z-index: 1;
  }
  .diff-head .glyph {
    font-family: var(--mono);
    font-size: 0.875rem;
    font-weight: 600;
    color: var(--term-dim);
    flex: none;
  }
  .diff-head .path {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .diff-head .meta {
    font-size: 0.6875rem;
    color: var(--term-dim);
    margin-left: auto;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    flex: none;
  }
  .diff-head .add {
    color: var(--diff-add);
    font-weight: 600;
  }
  .diff-head .del {
    color: var(--diff-del);
    font-weight: 600;
    margin-left: 4px;
  }
  .diff pre {
    font-family: var(--mono);
    font-size: var(--r1);
    line-height: 1.5;
    padding: 0;
    margin: 0;
  }
  .dl {
    display: block;
    padding: 0 16px;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .dl .gut {
    box-sizing: content-box;
    display: inline-block;
    width: 4ch;
    color: var(--term-dim);
    user-select: none;
    text-align: right;
    padding-right: 12px;
    white-space: nowrap;
  }
  /* Hunk header + add/del rows: literal in-terminal accents tuned for the deep
     ink (same family as the terminal's ANSI greens/reds; doc-design/colors.md). */
  .dl.hunk {
    color: oklch(80% 0.10 280);
    background: oklch(80% 0.10 280 / 0.12);
  }
  .dl.add {
    background: oklch(82% 0.13 150 / 0.1);
    color: oklch(82% 0.13 150);
  }
  .dl.del {
    background: oklch(78% 0.13 28 / 0.1);
    color: oklch(78% 0.13 28);
  }
  .dl.ctx {
    color: var(--term-dim);
  }

  .diff-empty {
    display: grid;
    place-content: center;
    height: calc(100% - 38px);
    padding: 24px;
  }
  .diff-empty p {
    font-family: var(--ui);
    font-size: var(--r0);
    color: var(--term-dim);
  }
</style>
