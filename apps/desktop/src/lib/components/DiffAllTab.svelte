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
  import {
    FileDiff,
    processPatch,
    type FileDiffMetadata,
    type FileDiffOptions,
  } from "@pierre/diffs";
  import ChevronDown from "~icons/lucide/chevron-down";
  import ChevronRight from "~icons/lucide/chevron-right";
  import { allChangesFiles } from "../daemon";
  import { parseDiff } from "../diff";
  import { fileIconUrl } from "../file-icons";
  import { theme } from "../theme";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import { diffStyle, diffWrap } from "../settings";

  // Collapsed sections by row identity. Absent / false = expanded (the default).
  // All-changes can contain the same path twice (staged + unstaged), so caches
  // must include stagedness instead of using file.path alone.
  let collapsed = $state<Record<string, boolean>>({});

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

  function allChangesRowKey(path: string, staged: boolean): string {
    return `${staged ? "staged" : "unstaged"}\0${path}`;
  }

  // Render-side view options shared by every per-file instance. Driven by the
  // persisted `diffStyle` / `diffWrap` settings (same as DiffTab) — these only
  // re-lay-out already-fetched diffs, so a change applies via setOptions +
  // rerender on each live instance rather than re-fetching. `$derived` so
  // sections mounted (expanded) after a change start with the current values.
  const options = $derived<FileDiffOptions<undefined>>({
    diffStyle: $diffStyle,
    disableFileHeader: true, // each section renders its own header row.
    disableLineNumbers: false,
    diffIndicators: "classic",
    hunkSeparators: "line-info",
    lineDiffType: "word",
    overflow: $diffWrap ? "wrap" : "scroll",
    stickyHeader: false,
    preferredHighlighter: "shiki-js", // no WASM, faster startup.
    theme: { light: "pierre-light", dark: "pierre-dark" },
    themeType: $theme,
  });

  function toggle(rowKey: string) {
    collapsed[rowKey] = !collapsed[rowKey];
  }

  // A partially-staged file contributes two rows (staged + unstaged); the
  // header counts distinct files, not sections.
  const fileCount = $derived(new Set($allChangesFiles.map((f) => f.path)).size);

  // A single FileDiff per mounted (expanded, renderable) section. The Svelte
  // action owns its instance: it renders on mount + on metadata/theme change and
  // cleans up when the section unmounts (collapse) — cleanUp() detaching the
  // <diffs-container> is fine here because Svelte is removing it too. Mirrors the
  // teardown reasoning in DiffTab, but per section. The render-side `opts` are
  // threaded through the action params so a split/wrap toggle re-applies to
  // every active instance via setOptions + rerender (the {@const} below passes
  // the current `options` to each section, so Svelte re-runs `update` on change).
  type ViewParams = {
    fileDiff: FileDiffMetadata;
    opts: FileDiffOptions<undefined>;
  };

  function fileDiffView(node: HTMLElement, params: ViewParams) {
    let instance: FileDiff<undefined> | undefined;
    let lastFileDiff: FileDiffMetadata | undefined;

    function render(fileDiff: FileDiffMetadata, opts: FileDiffOptions<undefined>) {
      if (!instance) instance = new FileDiff<undefined>({ ...opts });
      else instance.setOptions(opts);
      instance.setThemeType(opts.themeType ?? "light");
      instance.render({ fileDiff, fileContainer: node, forceRender: true });
      lastFileDiff = fileDiff;
    }

    render(params.fileDiff, params.opts);

    return {
      update(next: ViewParams) {
        if (next.fileDiff !== lastFileDiff) {
          // New metadata: a full re-render (which also picks up the latest opts).
          render(next.fileDiff, next.opts);
        } else if (instance) {
          // Same metadata, changed options (split/wrap/theme): merge + re-lay-out.
          instance.setOptions(next.opts);
          instance.setThemeType(next.opts.themeType ?? "light");
          instance.rerender();
        }
      },
      destroy() {
        instance?.cleanUp();
        instance = undefined;
      },
    };
  }
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
    {@const isCollapsed = collapsed[rowKey] === true}
    {@const parsed = parsedFor(rowKey, file.text, !isCollapsed)}
    {@const files = filesFor(rowKey, file.text, !isCollapsed)}
    {@const showSectionHeaders = files.length > 1}
    <section class="file">
      <button
        class="file-head"
        type="button"
        aria-expanded={!isCollapsed}
        onclick={() => toggle(rowKey)}
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

  /* The @pierre/diffs container — same chrome→terminal token bridge as DiffTab. */
  .diffs {
    display: block;
    --diffs-font-family: var(--mono);
    --diffs-font-size: var(--r1);
    --diffs-light-bg: var(--term-bg2);
    --diffs-dark-bg: var(--term-bg2);
    --diffs-bg-context-override: var(--term-bg2);
    --diffs-bg-addition-override: oklch(from var(--diff-add) l c h / 0.1);
    --diffs-bg-deletion-override: oklch(from var(--diff-del) l c h / 0.1);
    --diffs-bg-addition-emphasis-override: oklch(from var(--diff-add) l c h / 0.22);
    --diffs-bg-deletion-emphasis-override: oklch(from var(--diff-del) l c h / 0.22);
    --diffs-addition-color-override: var(--diff-add);
    --diffs-deletion-color-override: var(--diff-del);
    --diffs-fg-number-override: var(--term-dim);
    --diffs-bg-hover-override: oklch(from var(--term-fg) l c h / 0.06);
  }

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
