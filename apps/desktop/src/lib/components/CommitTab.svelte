<script lang="ts">
  // Commit Tab (the `\0commit:<sha>` diff tab): one immutable commit shown as a
  // metadata header above collapsible per-file diff sections, in the all-changes
  // reading surface. Opened from History (one tab per commit, keyed by sha).
  //
  // Data comes from `fetchCommitDiff(worktreeId, sha)` — an immutable per-sha
  // cache, so opening the same commit twice (or switching back to its tab) fetches
  // once and serves the cache forever (no invalidation path). We just call it on
  // mount; the cache + in-flight coalescing do the rest. The tab survives history
  // rewrites by design: an amended/rebased-away sha keeps rendering its cached
  // object — there is no staleness logic and no auto-close.
  //
  // Per-file sections reuse the SHARED @pierre/diffs renderer (lib/diffView.ts)
  // exactly as DiffAllTab does: processPatch splits a file's patch into sections,
  // and one FileDiff instance is mounted per section while expanded. parseDiff
  // supplies the per-file add/del counts and the binary/empty fallback states.
  import { onMount } from "svelte";
  import { processPatch, type FileDiffMetadata } from "@pierre/diffs";
  import Copy from "~icons/lucide/copy";
  import Check from "~icons/lucide/check";
  import { fetchCommitDiff, selectedWorktreeId, type CommitDiffData } from "../daemon";
  import { parseDiff } from "../diff";
  import { diffViewOptions } from "../diffView";
  import { theme } from "../theme";
  import { STATUS_GLYPH, statusGlyphClass } from "../types";
  import DiffFileSection from "./DiffFileSection.svelte";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import ExpandAllToggle from "./ExpandAllToggle.svelte";
  import { diffStyle, diffWrap } from "../settings";

  let { sha }: { sha: string } = $props();

  // The sticky commit header's measured height. Unlike DiffAllTab's fixed 38px
  // bar, this header is a multi-line column (sha + summary + body + byline) whose
  // height depends on the commit message, so we feed the live height to the
  // per-file sections' sticky offset (--section-sticky-top) — otherwise each file
  // head would pin at the 38px default, inside this taller header's footprint,
  // and paint behind it.
  let headHeight = $state(0);

  // The commit's worktree is the one selected when the tab is opened/mounted.
  // The per-sha cache keys on worktree+sha, so capturing it once on mount is
  // correct: this tab instance is keyed by sha in Center, so a different commit
  // remounts a fresh instance.
  const worktreeId = $state.snapshot($selectedWorktreeId);

  // null = loading (no fetch result yet); a resolved object = loaded; the `failed`
  // flag marks a fetch that returned null (daemon error) so we can show an error
  // state distinct from loading. Matches DiffAllTab's loading idiom (a null body
  // reads as "Loading"), extended with an error terminal.
  let data = $state<CommitDiffData | null>(null);
  let failed = $state(false);

  onMount(() => {
    if (worktreeId === null) {
      failed = true;
      return;
    }
    void fetchCommitDiff(worktreeId, sha).then((result) => {
      if (result === null) failed = true;
      else data = result;
    });
  });

  // Render-side view options shared by every per-file instance — identical to
  // DiffTab/DiffAllTab (split/wrap/theme only re-lay-out an already-fetched diff).
  const options = $derived(diffViewOptions($diffStyle, $diffWrap, $theme));

  // Per-file expanded set, keyed by path. Local to this tab instance (no daemon
  // store: commit diffs are immutable and fully delivered up front, so there is
  // nothing to lazily refetch — expanding is purely a render concern). Absent =
  // collapsed (the default), mirroring all-changes; the head's expand-all toggle
  // or a per-section click opens sections.
  let expanded = $state(new Set<string>());

  function toggle(path: string) {
    const next = new Set(expanded);
    if (next.has(path)) next.delete(path);
    else next.add(path);
    expanded = next;
  }

  // The head's expand/collapse-all toggle: any expanded section flips the action
  // to "collapse all". Expand-all is render-only (every file's diff text is
  // already here), so it just fills the set with the commit's paths.
  const anyExpanded = $derived(expanded.size > 0);

  function toggleAll() {
    expanded = anyExpanded ? new Set() : new Set((data?.files ?? []).map((file) => file.path));
  }

  // Parsed-diff + pierre-section caches keyed by path (the commit's file list is
  // stable for the tab's life, so these never need invalidation). Only computed
  // for expanded files, like all-changes.
  type ParsedDiff = ReturnType<typeof parseDiff>;
  const parsedByPath = new Map<string, ParsedDiff>();
  const filesByPath = new Map<string, FileDiffMetadata[]>();

  function parsedFor(path: string, text: string): ParsedDiff {
    const cached = parsedByPath.get(path);
    if (cached) return cached;
    const parsed = parseDiff(text);
    parsedByPath.set(path, parsed);
    return parsed;
  }

  function filesFor(path: string, text: string): FileDiffMetadata[] {
    const cached = filesByPath.get(path);
    if (cached) return cached;
    const files = processPatch(text, undefined).files;
    filesByPath.set(path, files);
    return files;
  }

  // Absolute, readable date from the commit's unix-seconds `time`. No app-wide
  // date util exists; a locale medium date + short time reads cleanly and
  // localizes for free.
  function formatDate(unixSeconds: number): string {
    return new Date(unixSeconds * 1000).toLocaleString(undefined, {
      dateStyle: "medium",
      timeStyle: "short",
    });
  }

  // Click-to-copy the full sha with a brief "copied" confirmation (mirrors the
  // quiet, no-error copy idiom used elsewhere — ProjectTree.copyPath).
  let copied = $state(false);
  let copiedTimer: ReturnType<typeof setTimeout> | null = null;
  async function copySha() {
    try {
      await navigator.clipboard.writeText(sha);
      copied = true;
      if (copiedTimer) clearTimeout(copiedTimer);
      copiedTimer = setTimeout(() => (copied = false), 1500);
    } catch {
      // Clipboard can be unavailable (no focus / denied); a silent no-op beats a
      // scary error for a convenience action.
    }
  }
</script>

<div class="commit" style:--section-sticky-top="{headHeight}px">
  <div class="commit-head" bind:clientHeight={headHeight}>
    <div class="head-top">
      <span class="glyph" aria-hidden="true">±</span>
      <button class="sha" type="button" title="Copy full SHA" onclick={() => void copySha()}>
        <span class="sha-text">{sha}</span>
        <span class="sha-ico" aria-hidden="true">
          {#if copied}<Check />{:else}<Copy />{/if}
        </span>
        {#if copied}<span class="copied">copied</span>{/if}
      </button>
      {#if data?.meta.is_merge}
        <span class="badge merge">merge</span>
      {/if}
      <span class="head-right">
        {#if data}
          <span class="counts">
            <span class="add">+{data.meta.additions}</span>
            <span class="del">−{data.meta.deletions}</span>
          </span>
        {/if}
        {#if data && data.files.length > 0}
          <ExpandAllToggle {anyExpanded} onToggle={toggleAll} />
        {/if}
        <!-- Commit diffs are immutable per-sha snapshots with no re-fetch path
             (no ws/ctx on CommitDiffRequest, skipped by refreshOpenDiffs), so we
             show only the render-only controls (style/wrap). -->
        <DiffViewOptions rediff={false} />
      </span>
    </div>

    {#if data}
      {#if data.meta.summary}
        <p class="summary">{data.meta.summary}</p>
      {/if}
      {#if data.meta.body}
        <pre class="body">{data.meta.body}</pre>
      {/if}
      <div class="byline">
        {#if data.meta.author}<span class="author">{data.meta.author}</span>{/if}
        <span class="date">{formatDate(data.meta.time)}</span>
      </div>
    {/if}
  </div>

  {#if data === null}
    {#if failed}
      <div class="diff-empty"><p>Couldn’t load this commit.</p></div>
    {:else}
      <div class="diff-empty"><p>Loading commit…</p></div>
    {/if}
  {:else if data.files.length === 0}
    <div class="diff-empty"><p>No file changes in this commit.</p></div>
  {:else}
    {#each data.files as file (file.path)}
      {@const isCollapsed = !expanded.has(file.path)}
      <!-- The parse + section caches stay here (keyed by path — a commit is
           immutable, so they never invalidate), and the per-file collapsible
           markup lives in the shared DiffFileSection. The per-file status glyph is
           passed as the status-mark snippet; commit files have no trailing badge. -->
      <DiffFileSection
        path={file.path}
        parsed={!isCollapsed ? parsedFor(file.path, file.diff) : null}
        files={!isCollapsed ? filesFor(file.path, file.diff) : []}
        {isCollapsed}
        {options}
        onToggle={() => toggle(file.path)}
      >
        {#snippet statusMark()}
          <span class="st {statusGlyphClass(file.status)}" title={file.status} aria-hidden="true"
            >{STATUS_GLYPH[file.status]}</span
          >
        {/snippet}
      </DiffFileSection>
    {/each}
  {/if}
</div>

<style>
  /* Same edge-to-edge terminal surface as DiffTab/DiffAllTab so the Commit Tab
     reads as a sibling of the existing diff views. */
  .commit {
    height: 100%;
    width: 100%;
    background: var(--term-bg2);
    overflow-y: auto;
  }

  /* Metadata header bar: a sibling of the DiffTab/DiffAllTab head, but stacked —
     a top line (sha + merge badge + totals) above the message, byline. */
  .commit-head {
    display: flex;
    flex-direction: column;
    gap: 8px;
    padding: 9px 16px 11px;
    border-bottom: 1px solid var(--term-line);
    position: sticky;
    top: 0;
    background: var(--term-bg2);
    z-index: 2;
  }
  .head-top {
    display: flex;
    align-items: center;
    gap: 9px;
  }
  .commit-head .glyph {
    font-family: var(--mono);
    font-size: 0.875rem;
    font-weight: 600;
    color: var(--term-dim);
    flex: none;
  }

  /* Click-to-copy full sha: mono, dim, with a quiet copy affordance + "copied"
     confirmation. Bare button so it reads as the header sha, not a control. */
  .sha {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    background: transparent;
    border: 0;
    border-radius: 0;
    padding: 0;
    cursor: pointer;
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    transition: color 0.15s ease-out;
  }
  .sha-text {
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .sha-ico {
    display: inline-grid;
    place-items: center;
    width: 13px;
    height: 13px;
    flex: none;
    color: var(--term-dim);
  }
  .sha-ico :global(svg) {
    width: 13px;
    height: 13px;
    stroke-width: 1.5px;
  }
  .sha:hover {
    color: var(--term-fg);
  }
  .sha:hover .sha-ico {
    color: var(--term-fg);
  }
  .sha:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: 2px;
  }
  .copied {
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--term-dim);
    flex: none;
  }

  .badge.merge {
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--term-dim);
    border: 1px solid var(--term-line);
    padding: 1px 5px;
    flex: none;
  }

  .head-right {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-left: auto;
    flex: none;
  }
  /* Header ± totals + per-file counts share one styling, matching DiffTab. */
  .counts {
    font-size: 0.6875rem;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    flex: none;
  }
  .add {
    color: var(--diff-add);
    font-weight: 600;
  }
  .del {
    color: var(--diff-del);
    font-weight: 600;
    margin-left: 4px;
  }

  .summary {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    font-weight: 600;
    margin: 0;
    line-height: 1.4;
  }
  .body {
    font-family: var(--mono);
    font-size: var(--r0);
    color: var(--term-dim);
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    line-height: 1.5;
  }
  .byline {
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--term-dim);
    font-variant-numeric: tabular-nums;
  }
  .byline .author {
    color: var(--term-fg);
  }

  /* The collapsible per-file section (head button, fallback branches, per-section
     @pierre/diffs rendering) and its CSS now live in the shared DiffFileSection.
     The status glyph below is the only per-section chrome owned here: it's passed
     as a snippet, so it renders in this component's style scope rather than the
     child's — hence a standalone `.st` selector instead of `.file-head .st`.
     Same letters/hues as the Changes rail, retinted to the diff surface where
     needed (U falls back to the dim terminal token). Each status has an explicit
     color, so it does not pick up the head's hover recolor (unchanged from before,
     where these same per-status colors won over the inherited hover color). */
  .st {
    width: 13px;
    text-align: center;
    font-weight: 700;
    font-size: 0.75rem;
    font-family: var(--mono);
    flex: 0 0 13px;
  }
  .st.M {
    color: var(--st-stall);
  }
  .st.A {
    color: var(--st-ok);
  }
  .st.D {
    color: var(--diff-del);
  }
  .st.U {
    color: var(--term-dim);
  }

  /* Tab-level state ("Loading commit…", "Couldn't load this commit.", "No file
     changes in this commit."), centered full-height — distinct from the compact
     in-section fallbacks owned by DiffFileSection. */
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
