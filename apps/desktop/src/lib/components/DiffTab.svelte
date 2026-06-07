<script lang="ts">
  // Diff view (mockup .diff): a unified diff rendered with Shiki syntax
  // highlighting via @pierre/diffs. The daemon-fetched diff text (a full git
  // unified-diff string) is parsed by @pierre/diffs' processPatch — which splits
  // the patch on `diff --git` boundaries — and each file section is rendered into
  // its own shadow-DOM <diffs-container> by a FileDiff instance. Most rows are a
  // single file (one section, identical to a plain FileDiff), but a collapsed
  // untracked-directory row diffs with recurse_untracked_dirs(true) on the daemon
  // side, so its patch carries one `diff --git` section per file in the directory;
  // processPatch yields them all so the whole directory renders rather than only
  // the first file (processFile would greedily fold the later files' hunks into
  // the first, corrupting the view). The local classifier (lib/diff.ts) is still
  // used for the add/del counts and the empty/binary fallback states. Theming
  // follows the app's light/dark mode and bridges Pierre's --diffs-* chrome vars
  // to the app's --term-* tokens. Peer of the session terminal in the center; the
  // tab + close affordance live in SessionTabs.
  // Importing FileDiff also registers the <diffs-container> custom element it
  // renders into (its module pulls in @pierre/diffs' web-components side effect,
  // which owns the shadow root + adopted Pierre stylesheet).
  import { processPatch, type FileDiffMetadata } from "@pierre/diffs";
  import { activeDiffPath, diffText } from "../daemon";
  import { parseDiff } from "../diff";
  import { diffViewOptions, fileDiffView } from "../diffView";
  import { theme } from "../theme";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import { diffStyle, diffWrap } from "../settings";

  // Local classifier: only used here for the add/del counts shown in the header
  // and to detect the binary / empty (mode/rename-only) cases that processPatch
  // also returns no renderable files for — keeping the existing fallback UI.
  const parsed = $derived($diffText === null ? null : parseDiff($diffText));

  // Pierre's per-file metadata, one entry per `diff --git` section in the patch.
  // A normal file row yields a single section; a collapsed untracked-directory
  // row yields one section per file in the directory. Empty for binary/empty.
  const files = $derived<FileDiffMetadata[]>(
    $diffText === null ? [] : processPatch($diffText, undefined).files,
  );
  // True when there's at least one renderable file section.
  const hasDiff = $derived(files.length > 0);
  // Show a per-section path header only when the patch spans multiple files
  // (a directory row); a single-file diff keeps the bare top header bar.
  const showSectionHeaders = $derived(files.length > 1);

  // Render-side view options driven by the persisted `diffStyle` / `diffWrap`
  // settings. They only re-lay-out the already-fetched diff (split vs unified,
  // wrap vs scroll), so changing them calls setOptions + rerender on the live
  // instances rather than re-fetching. `$derived` so sections created mid-
  // session also start with the current values. The instance lifecycle + the
  // chrome token bridge live in the shared diffView module (fileDiffView action).
  const options = $derived(diffViewOptions($diffStyle, $diffWrap, $theme));
</script>

<div class="diff">
  <div class="diff-head">
    <span class="glyph" aria-hidden="true">±</span>
    <span class="path">{$activeDiffPath}</span>
    <span class="head-right">
      {#if parsed && !parsed.isEmpty}
        <span class="meta">
          <span class="add">+{parsed.additions}</span>
          <span class="del">−{parsed.deletions}</span>
        </span>
      {/if}
      <DiffViewOptions />
    </span>
  </div>

  {#if parsed === null}
    <div class="diff-empty"><p>Loading diff…</p></div>
  {:else if parsed.isBinary}
    <div class="diff-empty"><p>Binary file — no text diff.</p></div>
  {:else if parsed.isEmpty || !hasDiff}
    <div class="diff-empty"><p>No textual changes.</p></div>
  {:else}
    <!-- One @pierre/diffs instance per file section in the patch. A single file
         renders one bare section; a directory row stacks every file, each with a
         small path header so they read as distinct files. The action keys off the
         metadata identity so re-renders rebuild only what changed. -->
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
</div>

<style>
  /* The diff is the center peer of the terminal: it sits on the same edge-to-
     edge deep-ink panel so closing it reveals the terminal seamlessly. */
  .diff {
    height: 100%;
    width: 100%;
    /* Flat fill matching the terminal panel (Terminal.svelte dropped the
       bg2→bg gradient — a gradient seams against flat sibling surfaces). */
    background: var(--term-bg2);
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
  .diff-head .head-right {
    display: flex;
    align-items: center;
    gap: 12px;
    margin-left: auto;
    flex: none;
  }
  .diff-head .meta {
    font-size: 0.6875rem;
    color: var(--term-dim);
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

  /* Per-file section header for multi-file (directory) rows. Matches the
     all-changes section header (DiffAllTab .file-head) so stacked files read the
     same across both diff views. Single-file diffs never render this. */
  .section + .section {
    border-top: 1px solid var(--term-line);
  }
  .section-head {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 7px 16px;
    background: var(--term-bg2);
    border-bottom: 1px solid var(--term-line);
    position: sticky;
    top: 38px;
    z-index: 1;
  }
  .section-head .fpath {
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--term-fg);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }

  /* The @pierre/diffs container's chrome→terminal token bridge (the --diffs-*
     custom properties) is applied imperatively by the shared fileDiffView action
     in lib/diffView.ts so it lives in one place; `display: block` is set there. */

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
