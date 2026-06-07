<script lang="ts">
  // One collapsible per-file diff section, shared by the all-changes reading
  // surfaces (DiffAllTab and CommitTab). Both rendered byte-identical machinery:
  // a sticky `.file-head` button (chevron + file icon + path + add/del counts),
  // the loading / binary / empty fallback branches, and a `{#each files}` loop
  // that mounts one @pierre/diffs FileDiff per `diff --git` section via the shared
  // `fileDiffView` action (lib/diffView.ts). This component owns that markup + its
  // scoped CSS so the two call sites stop drifting (CommitTab's old comments
  // literally said "mirrors DiffAllTab").
  //
  // The genuine divergences between the two call sites are parameterized rather
  // than flattened:
  //   - the parsed-diff + section caches stay at the call site: CommitTab keys by
  //     path (an immutable commit, never invalidated), DiffAllTab keys by row and
  //     tracks the diff text (a live, refetchable status row). So this component
  //     takes the already-resolved `parsed` / `files` rather than parsing itself.
  //   - collapse state lives at the call site too (CommitTab's local Set vs
  //     DiffAllTab's daemon-backed store); we only receive `isCollapsed` + an
  //     `onToggle` callback.
  //   - the head's optional status mark (CommitTab's `.st` glyph) and the optional
  //     trailing badge (DiffAllTab's `staged` tag) are passed as snippets, so a
  //     call site that needs neither renders neither.
  import type { Snippet } from "svelte";
  import type { FileDiffMetadata, FileDiffOptions } from "@pierre/diffs";
  import ChevronDown from "~icons/lucide/chevron-down";
  import ChevronRight from "~icons/lucide/chevron-right";
  import type { ParsedDiff } from "../diff";
  import { fileDiffView } from "../diffView";
  import { fileIconUrl } from "../file-icons";

  let {
    // The file path shown in the head and used for its icon. For all-changes rows
    // this is the row path; for commit files the commit file path.
    path,
    // The local classifier's result for this file: `null` while the diff text is
    // still loading (DiffAllTab's cold rows), else the add/del counts + the
    // binary/empty flags that drive the fallback branches. Resolved by the caller
    // so each keeps its own parse cache.
    parsed,
    // Pierre's per-file metadata, one entry per `diff --git` section in this file's
    // patch (a single entry for a normal file, one per file for a collapsed
    // untracked-directory row). Empty while collapsed or for binary/empty patches.
    files,
    // Whether this section is collapsed (its <diffs-container>s unmounted). Owned
    // by the caller's collapse state.
    isCollapsed,
    // Render-side @pierre/diffs options, threaded to every section instance so a
    // split/wrap/theme toggle re-lays-out the live instances.
    options,
    // Toggle handler for the head button (expand/collapse).
    onToggle,
    // Optional status mark rendered between the chevron and the file icon
    // (CommitTab's per-file status glyph). Omitted by all-changes.
    statusMark,
    // Optional trailing tag rendered after the path, before the counts
    // (DiffAllTab's `staged` badge). Omitted by the commit view.
    badge,
  }: {
    path: string;
    parsed: ParsedDiff | null;
    files: FileDiffMetadata[];
    isCollapsed: boolean;
    options: FileDiffOptions<undefined>;
    onToggle: () => void;
    statusMark?: Snippet;
    badge?: Snippet;
  } = $props();

  // Show a per-section path sub-header only when this file's patch spans more than
  // one `diff --git` section (a collapsed untracked-directory row); a normal
  // single-file section renders the bare diff body with no sub-header.
  const showSectionHeaders = $derived(files.length > 1);
</script>

<section class="file">
  <button class="file-head" type="button" aria-expanded={!isCollapsed} onclick={onToggle}>
    <span class="chev" aria-hidden="true">
      {#if isCollapsed}
        <ChevronRight />
      {:else}
        <ChevronDown />
      {/if}
    </span>
    {#if statusMark}{@render statusMark()}{/if}
    <span class="ftype" aria-hidden="true"><img src={fileIconUrl(path)} alt="" /></span>
    <span class="fpath">{path}</span>
    {#if badge}{@render badge()}{/if}
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
      <!-- One @pierre/diffs instance per file section in the patch, mounted only
           while expanded so collapsed sections are never processed. A normal file
           is a single section (no sub-header); a collapsed untracked-directory row
           stacks every file, each with a small path sub-header so they read as
           distinct files rather than repeating the directory path. -->
      {#each files as fileDiff (fileDiff.name)}
        <div class="section">
          {#if showSectionHeaders}
            <div class="section-head">
              <span class="fpath">{fileDiff.name}</span>
            </div>
          {/if}
          <diffs-container class="diffs" use:fileDiffView={{ fileDiff, opts: options }}
          ></diffs-container>
        </div>
      {/each}
    {/if}
  {/if}
</section>

<style>
  .file {
    border-bottom: 1px solid var(--term-line);
  }
  /* Collapsible section header: chevron + (optional status mark) + file icon +
     path + (optional badge) + counts. Sticks just under the surrounding head
     (the all-changes / commit metadata bar) so the current file's name stays
     visible while scrolling its body.

     The sticky offset is a variable because the surrounding head's height is
     call-site-dependent: DiffAllTab's single-line bar is a fixed 38px (the
     fallback), but CommitTab's metadata bar is a multi-line column whose height
     grows with the commit message, so it measures its own header and sets
     `--section-sticky-top` on the scroll container. Without this the file head
     would pin at 38px inside a taller commit header and paint behind it
     (z-index 1 vs the head's 2), occluding the current file's name. */
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
    top: var(--section-sticky-top, 38px);
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
     normal single-file row renders the bare diff body with no sub-header. Matches
     DiffTab's .section-head so stacked files read the same across the diff views. */
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

  /* In-section fallback states (loading / binary / empty). The tab-level empty
     state (no files at all) is rendered + styled by each call site, since its
     full-height centering differs from this compact in-section variant. */
  .diff-empty.small {
    height: auto;
    place-content: start;
    padding: 12px 16px 16px 38px;
    display: grid;
  }
  .diff-empty.small p {
    font-family: var(--ui);
    font-size: var(--r0);
    color: var(--term-dim);
  }
</style>
