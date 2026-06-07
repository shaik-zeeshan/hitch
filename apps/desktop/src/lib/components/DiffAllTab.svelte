<script lang="ts">
  // All-changes view (the special ALL_CHANGES_TAB diff tab): every changed file
  // in one scroll, each as a collapsible section. Mirrors DiffTab's @pierre/diffs
  // rendering, but one FileDiff instance per *expanded* file section (the library
  // supports many instances). Sections start collapsed (the default — the head's
  // expand-all toggle or a per-section click opens them); a collapsed section's
  // <diffs-container> is unmounted so we only process diffs the user is looking
  // at. Per-row diff text comes from `allChangesFiles` (fanned out by
  // viewAllChanges in daemon.ts), not a single diff string — so this reuses the
  // --diffs-* token bridge from DiffTab but its own data path.
  //
  // A row's patch usually carries one `diff --git` section, but a collapsed
  // untracked-directory row diffs with recurse_untracked_dirs(true) on the daemon
  // side, so its patch carries one section per file in the directory. We parse
  // each row's text with processPatch and render one FileDiff per section (one
  // path sub-header each, mirroring DiffTab) — processFile is single-file-only and
  // would greedily fold the later files' hunks into the first, corrupting the view.
  import { processPatch, type FileDiffMetadata } from "@pierre/diffs";
  import {
    allChangesExpanded,
    allChangesFiles,
    allChangesRowKey,
    fetchAllChangesRow,
    setAllChangesAllExpanded,
  } from "../daemon";
  import { parseDiff } from "../diff";
  import { diffViewOptions } from "../diffView";
  import { theme } from "../theme";
  import DiffFileSection from "./DiffFileSection.svelte";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import ExpandAllToggle from "./ExpandAllToggle.svelte";
  import { diffStyle, diffWrap } from "../settings";

  // Expanded sections live in the daemon store (`allChangesExpanded`, keyed by
  // row identity) so a refresh only fetches diffs for rows the user has
  // expanded. Absent from the set = collapsed (the default). All-changes can
  // contain the same path twice (staged + unstaged), so the key includes
  // stagedness instead of using file.path alone.

  type ParsedDiff = ReturnType<typeof parseDiff>;
  const parsedByRow = new Map<string, { text: string; parsed: ParsedDiff }>();

  function parsedFor(rowKey: string, text: string | null, expanded: boolean): ParsedDiff | null {
    if (text === null) return null;
    const cached = parsedByRow.get(rowKey);
    if (cached?.text === text) return cached.parsed;
    if (!expanded) return null;
    const parsed = parseDiff(text);
    parsedByRow.set(rowKey, { text, parsed });
    return parsed;
  }

  // Pierre's per-file metadata for an expanded row, one entry per `diff --git`
  // section: a single entry for a normal file row, one per file for a collapsed
  // untracked-directory row. Cached by row identity so a re-render reuses the
  // parsed sections rather than re-splitting the patch. Only computed for
  // expanded rows (collapsed rows are never processed). processPatch returns no
  // files for binary/empty patches, which the parseDiff classifier already flags
  // and renders a fallback for, so this is only read on the renderable branch.
  const filesByRow = new Map<string, { text: string; files: FileDiffMetadata[] }>();

  function filesFor(rowKey: string, text: string | null, expanded: boolean): FileDiffMetadata[] {
    if (text === null || !expanded) return [];
    const cached = filesByRow.get(rowKey);
    if (cached?.text === text) return cached.files;
    const files = processPatch(text, undefined).files;
    filesByRow.set(rowKey, { text, files });
    return files;
  }

  // Render-side view options shared by every per-file instance. Driven by the
  // persisted `diffStyle` / `diffWrap` settings (same as DiffTab) — these only
  // re-lay-out already-fetched diffs, so a change applies via setOptions +
  // rerender on each live instance rather than re-fetching. `$derived` so
  // sections mounted (expanded) after a change start with the current values.
  // Built by the shared diffView module (same options as DiffTab).
  const options = $derived(diffViewOptions($diffStyle, $diffWrap, $theme));

  // Collapse/expand a section. Toggling the shared store lets the daemon's
  // refresh path know which rows to fetch; expanding a row that was collapsed
  // (so its diff may have been evicted by a status-poll refresh, or never
  // fetched) kicks an on-demand fetch through the diff cache — a warm row
  // resolves instantly, a cold one fills in from `null` (the "Loading" state).
  function toggle(file: { path: string; staged: boolean }) {
    const rowKey = allChangesRowKey(file.path, file.staged);
    const willExpand = !$allChangesExpanded.has(rowKey);
    allChangesExpanded.update((set) => {
      const next = new Set(set);
      if (next.has(rowKey)) next.delete(rowKey);
      else next.add(rowKey);
      return next;
    });
    if (willExpand) void fetchAllChangesRow(file.path, file.staged);
  }

  // A partially-staged file contributes two rows (staged + unstaged); the
  // header counts distinct files, not sections.
  const fileCount = $derived(new Set($allChangesFiles.map((f) => f.path)).size);

  // The head's expand/collapse-all toggle: any expanded section flips the action
  // to "collapse all". The daemon helper owns the effect (set + bounded fan-out).
  const anyExpanded = $derived(
    $allChangesFiles.some((f) => $allChangesExpanded.has(allChangesRowKey(f.path, f.staged))),
  );

  // A single FileDiff per mounted (expanded, renderable) section. The shared
  // fileDiffView action (lib/diffView.ts) owns each instance: it renders on mount
  // + on metadata/theme change and cleans up when the section unmounts (collapse).
  // The render-side `opts` are threaded through the action params (the {@const}
  // below passes the current `options` to each section, so Svelte re-runs the
  // action's `update` on a split/wrap/theme change), re-applying via setOptions +
  // rerender to every active instance.
</script>

<div class="diffall">
  <div class="all-head">
    <span class="glyph" aria-hidden="true">±</span>
    <span class="path">All changes</span>
    <span class="head-right">
      <span class="meta">{fileCount} file{fileCount === 1 ? "" : "s"}</span>
      {#if $allChangesFiles.length > 0}
        <ExpandAllToggle {anyExpanded} onToggle={() => setAllChangesAllExpanded(!anyExpanded)} />
      {/if}
      <DiffViewOptions />
    </span>
  </div>

  {#if $allChangesFiles.length === 0}
    <div class="diff-empty"><p>No changes to show.</p></div>
  {/if}

  {#each $allChangesFiles as file (allChangesRowKey(file.path, file.staged))}
    {@const rowKey = allChangesRowKey(file.path, file.staged)}
    {@const isCollapsed = !$allChangesExpanded.has(rowKey)}
    <!-- The parse + section caches stay here (keyed by row + diff text, so a live
         status refetch re-parses), and the per-file collapsible markup lives in
         the shared DiffFileSection. The `staged` tag is passed as the trailing
         badge snippet; all-changes rows have no per-file status mark. -->
    <DiffFileSection
      path={file.path}
      parsed={parsedFor(rowKey, file.text, !isCollapsed)}
      files={filesFor(rowKey, file.text, !isCollapsed)}
      {isCollapsed}
      {options}
      onToggle={() => toggle(file)}
    >
      {#snippet badge()}
        {#if file.staged}
          <span class="badge">staged</span>
        {/if}
      {/snippet}
    </DiffFileSection>
  {/each}
</div>

<style>
  /* Same edge-to-edge terminal surface as DiffTab so closing reveals the
     terminal seamlessly. */
  .diffall {
    height: 100%;
    width: 100%;
    background: var(--term-bg2);
    overflow-y: auto;
  }
  .all-head {
    display: flex;
    align-items: center;
    gap: 9px;
    padding: 9px 16px;
    border-bottom: 1px solid var(--term-line);
    position: sticky;
    top: 0;
    background: var(--term-bg2);
    z-index: 2;
  }
  .all-head .glyph {
    font-family: var(--mono);
    font-size: 0.875rem;
    font-weight: 600;
    color: var(--term-dim);
    flex: none;
  }
  .all-head .path {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    font-weight: 600;
  }
  .all-head .head-right {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-left: auto;
    flex: none;
  }
  .all-head .meta {
    font-size: 0.6875rem;
    color: var(--term-dim);
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    flex: none;
  }

  /* The collapsible per-file section (head button, fallback branches, per-section
     @pierre/diffs rendering) and its CSS now live in the shared DiffFileSection.
     The `staged` tag below is the only per-section chrome owned here: it's passed
     as a snippet, so it renders in this component's style scope rather than the
     child's — hence a standalone selector instead of `.file-head .badge`. */
  .badge {
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--term-dim);
    border: 1px solid var(--term-line);
    padding: 1px 5px;
    flex: none;
  }

  /* Tab-level empty state ("No changes to show"), centered full-height — distinct
     from the compact in-section fallbacks owned by DiffFileSection. */
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
