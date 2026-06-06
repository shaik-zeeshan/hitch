<script lang="ts">
  // All-changes view (the special ALL_CHANGES_TAB diff tab): every changed file
  // in one scroll, each as a collapsible section. Mirrors DiffTab's @pierre/diffs
  // rendering, but one FileDiff instance per *expanded* file section (the library
  // supports many instances). Sections are expanded by default; collapsing a
  // section unmounts its <diffs-container> so we only process diffs the user is
  // looking at. Per-row diff text comes from `allChangesFiles` (fanned out by
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
  import ChevronDown from "~icons/lucide/chevron-down";
  import ChevronRight from "~icons/lucide/chevron-right";
  import {
    allChangesCollapsed,
    allChangesFiles,
    allChangesRowKey,
    fetchAllChangesRow,
  } from "../daemon";
  import { parseDiff } from "../diff";
  import { diffViewOptions, fileDiffView } from "../diffView";
  import { fileIconUrl } from "../file-icons";
  import { theme } from "../theme";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import { diffStyle, diffWrap } from "../settings";

  // Collapsed sections live in the daemon store (`allChangesCollapsed`, keyed by
  // row identity) so a refresh can skip fetching diffs for rows the user has
  // collapsed. Absent from the set = expanded (the default). All-changes can
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
    const willExpand = $allChangesCollapsed.has(rowKey);
    allChangesCollapsed.update((set) => {
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
      <DiffViewOptions />
    </span>
  </div>

  {#if $allChangesFiles.length === 0}
    <div class="diff-empty"><p>No changes to show.</p></div>
  {/if}

  {#each $allChangesFiles as file (allChangesRowKey(file.path, file.staged))}
    {@const rowKey = allChangesRowKey(file.path, file.staged)}
    {@const isCollapsed = $allChangesCollapsed.has(rowKey)}
    {@const parsed = parsedFor(rowKey, file.text, !isCollapsed)}
    {@const files = filesFor(rowKey, file.text, !isCollapsed)}
    {@const showSectionHeaders = files.length > 1}
    <section class="file">
      <button
        class="file-head"
        type="button"
        aria-expanded={!isCollapsed}
        onclick={() => toggle(file)}
      >
        <span class="chev" aria-hidden="true">
          {#if isCollapsed}
            <ChevronRight />
          {:else}
            <ChevronDown />
          {/if}
        </span>
        <span class="ftype" aria-hidden="true"><img src={fileIconUrl(file.path)} alt="" /></span>
        <span class="fpath">{file.path}</span>
        {#if file.staged}
          <span class="badge">staged</span>
        {/if}
        {#if parsed && !parsed.isEmpty}
          <span class="counts">
            <span class="add">+{parsed.additions}</span>
            <span class="del">−{parsed.deletions}</span>
          </span>
        {/if}
      </button>

      {#if !isCollapsed}
        {#if parsed === null}
          <div class="diff-empty small"><p>Loading diff…</p></div>
        {:else if parsed.isBinary}
          <div class="diff-empty small"><p>Binary file — no text diff.</p></div>
        {:else if parsed.isEmpty}
          <div class="diff-empty small"><p>No textual changes.</p></div>
        {:else}
          <!-- One @pierre/diffs instance per file section in the row's patch,
               mounted only while expanded so collapsed rows are never processed.
               A normal row is a single section (no sub-header — identical to
               before); a collapsed untracked-directory row stacks every file,
               each with a small path sub-header (mirroring DiffTab) so they read
               as distinct files rather than repeating the directory path. -->
          {#each files as fileDiff (fileDiff.name)}
            <div class="section">
              {#if showSectionHeaders}
                <div class="section-head">
                  <span class="fpath">{fileDiff.name}</span>
                </div>
              {/if}
              <diffs-container
                class="diffs"
                use:fileDiffView={{ fileDiff, opts: options }}
              ></diffs-container>
            </div>
          {/each}
        {/if}
      {/if}
    </section>
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

  .file {
    border-bottom: 1px solid var(--term-line);
  }
  /* Collapsible section header: chevron + file icon + path + counts. Sticks just
     under the all-changes head so the current file's name stays visible while
     scrolling its body. */
  .file-head {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    padding: 7px 16px;
    background: var(--term-bg2);
    border: 0;
    border-radius: 0;
    cursor: pointer;
    text-align: left;
    position: sticky;
    top: 38px;
    z-index: 1;
    transition: color 0.15s ease-out;
  }
  .file-head:hover {
    color: var(--term-fg);
  }
  .file-head:focus-visible {
    outline: 1px solid var(--iris-ink);
    outline-offset: -2px;
  }
  .file-head .chev {
    display: inline-grid;
    place-items: center;
    width: 14px;
    height: 14px;
    flex: none;
    color: var(--term-dim);
  }
  .file-head .chev :global(svg) {
    width: 14px;
    height: 14px;
    stroke-width: 1.5px;
  }
  .file-head .ftype {
    display: inline-grid;
    place-items: center;
    width: 16px;
    height: 16px;
    flex: none;
  }
  .file-head .ftype img {
    width: 16px;
    height: 16px;
    display: block;
  }
  .file-head .fpath {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .file-head .badge {
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--term-dim);
    border: 1px solid var(--term-line);
    padding: 1px 5px;
    flex: none;
  }
  .file-head .counts {
    margin-left: auto;
    font-size: 0.6875rem;
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    flex: none;
  }
  .file-head .add {
    color: var(--diff-add);
    font-weight: 600;
  }
  .file-head .del {
    color: var(--diff-del);
    font-weight: 600;
    margin-left: 4px;
  }

  /* Per-file section sub-header for multi-file (collapsed-directory) rows. Only
     rendered when a row's patch spans more than one `diff --git` section; a
     normal single-file row renders the bare diff body with no sub-header, exactly
     as before. Matches DiffTab's .section-head so stacked files read the same
     across both diff views. */
  .section + .section {
    border-top: 1px solid var(--term-line);
  }
  .section-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 16px 7px 38px;
    background: var(--term-bg2);
    border-bottom: 1px solid var(--term-line);
  }
  .section-head .fpath {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The @pierre/diffs container's chrome→terminal token bridge is applied
     imperatively by the shared fileDiffView action (lib/diffView.ts), same as
     DiffTab; `display: block` is set there too. */

  .diff-empty {
    display: grid;
    place-content: center;
    height: calc(100% - 38px);
    padding: 24px;
  }
  .diff-empty.small {
    height: auto;
    place-content: start;
    padding: 12px 16px 16px 38px;
  }
  .diff-empty p {
    font-family: var(--ui);
    font-size: var(--r0);
    color: var(--term-dim);
  }
</style>
