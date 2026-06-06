<script lang="ts">
  // All-changes view (the special ALL_CHANGES_TAB diff tab): every changed file
  // in one scroll, each as a collapsible section. Mirrors DiffTab's @pierre/diffs
  // rendering, but one FileDiff instance per *expanded* file (the library
  // supports many instances). Sections are expanded by default; collapsing a
  // section unmounts its <diffs-container> so we only process diffs the user is
  // looking at. Per-file diff text comes from `allChangesFiles` (fanned out by
  // viewAllChanges in daemon.ts), not a single diff string — so this reuses the
  // --diffs-* token bridge from DiffTab but its own data path.
  import { FileDiff, processFile, type FileDiffOptions } from "@pierre/diffs";
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
  // action owns its instance: it renders on mount + on text/theme change and
  // cleans up when the section unmounts (collapse) — cleanUp() detaching the
  // <diffs-container> is fine here because Svelte is removing it too. Mirrors the
  // teardown reasoning in DiffTab, but per section. The render-side `opts` are
  // threaded through the action params so a split/wrap toggle re-applies to
  // every active instance via setOptions + rerender (the {@const} below passes
  // the current `options` to each section, so Svelte re-runs `update` on change).
  type ViewParams = {
    text: string;
    themeType: "light" | "dark";
    opts: FileDiffOptions<undefined>;
  };

  function fileDiffView(node: HTMLElement, params: ViewParams) {
    let instance: FileDiff<undefined> | undefined;
    let lastText: string | null = null;

    function render(text: string, opts: FileDiffOptions<undefined>) {
      const fileDiff = processFile(text, { isGitDiff: true });
      if (!fileDiff) return;
      if (!instance) instance = new FileDiff<undefined>({ ...opts });
      else instance.setOptions(opts);
      instance.setThemeType(opts.themeType ?? "light");
      instance.render({ fileDiff, fileContainer: node, forceRender: true });
      lastText = text;
    }

    render(params.text, params.opts);

    return {
      update(next: ViewParams) {
        if (next.text !== lastText) {
          // New text: a full re-render (which also picks up the latest opts).
          render(next.text, next.opts);
        } else if (instance) {
          // Same text, changed options (split/wrap/theme): merge + re-lay-out.
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
          <!-- One @pierre/diffs instance per expanded section, mounted only while
               expanded so collapsed files are never processed. -->
          <diffs-container
            class="diffs"
            use:fileDiffView={{ text: file.text!, themeType: $theme, opts: options }}
          ></diffs-container>
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
