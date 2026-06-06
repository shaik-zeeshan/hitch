<script lang="ts">
  // Diff view (mockup .diff): a unified diff rendered with Shiki syntax
  // highlighting via @pierre/diffs. The daemon-fetched diff text (a full git
  // unified-diff string) is parsed by @pierre/diffs' processFile and rendered
  // into a shadow-DOM <diffs-container> by a FileDiff instance. The local
  // classifier (lib/diff.ts) is still used for the add/del counts and the
  // empty/binary fallback states. Theming follows the app's light/dark mode and
  // bridges Pierre's --diffs-* chrome vars to the app's --term-* tokens. Peer of
  // the session terminal in the center; the tab + close affordance live in
  // SessionTabs.
  // Importing FileDiff also registers the <diffs-container> custom element it
  // renders into (its module pulls in @pierre/diffs' web-components side effect,
  // which owns the shadow root + adopted Pierre stylesheet).
  import { FileDiff, processFile, type FileDiffOptions } from "@pierre/diffs";
  import { diffPath, diffText } from "../daemon";
  import { parseDiff } from "../diff";
  import { theme } from "../theme";
  import DiffViewOptions from "./DiffViewOptions.svelte";
  import { diffStyle, diffWrap } from "../settings";

  // Local classifier: only used here for the add/del counts shown in the header
  // and to detect the binary / empty (mode/rename-only) cases that processFile
  // also returns `undefined` for — keeping the existing fallback UI.
  const parsed = $derived($diffText === null ? null : parseDiff($diffText));

  // Pierre's FileDiffMetadata (undefined for empty/binary).
  const fileDiff = $derived(
    $diffText === null ? undefined : processFile($diffText, { isGitDiff: true }),
  );

  // Render-side view options driven by the persisted `diffStyle` / `diffWrap`
  // settings. They only re-lay-out the already-fetched diff (split vs unified,
  // wrap vs scroll), so changing them calls setOptions + rerender on the live
  // instance rather than re-fetching. `$derived` so instances created mid-
  // session also start with the current values.
  const options = $derived<FileDiffOptions<undefined>>({
    diffStyle: $diffStyle,
    disableFileHeader: true, // DiffTab renders its own header bar.
    disableLineNumbers: false,
    diffIndicators: "classic",
    hunkSeparators: "line-info",
    lineDiffType: "word",
    overflow: $diffWrap ? "wrap" : "scroll",
    stickyHeader: false,
    preferredHighlighter: "shiki-js", // no WASM, faster startup.
    theme: { light: "pierre-light", dark: "pierre-dark" },
  });

  let container = $state<HTMLElement>();
  let instance: FileDiff<undefined> | undefined;

  // Create the instance once, (re)render whenever the parsed diff changes.
  // Deliberately NO teardown on this effect: an $effect teardown runs before
  // every re-run (not just on destroy), and FileDiff.cleanUp() removes the
  // container element from the DOM (it assumes it created it, but ours is the
  // Svelte-owned <diffs-container> below) — cleaning up between renders would
  // leave every render after the first in a detached node, so only the first
  // clicked diff would ever show. When there's no renderable diff (empty /
  // binary / loading) the previous render is left in place, hidden by
  // `class:hidden` so only the fallback markup shows.
  $effect(() => {
    const el = container;
    if (!el || !fileDiff) return;
    if (!instance) instance = new FileDiff<undefined>({ ...options, themeType: $theme });
    instance.setThemeType($theme);
    instance.render({ fileDiff, fileContainer: el, forceRender: true });
  });

  // Destroy-only teardown: this effect reads no reactive state, so it runs
  // exactly once and its cleanup fires only on unmount (where cleanUp()
  // detaching the container is fine — Svelte is removing it anyway).
  $effect(() => {
    return () => {
      instance?.cleanUp();
      instance = undefined;
    };
  });

  // Follow the app's light/dark mode.
  $effect(() => {
    instance?.setThemeType($theme);
  });

  // Apply render-side option changes (split/unified, wrap/scroll) to the live
  // instance. setOptions only merges the new options; rerender() re-lays-out the
  // already-parsed diff with them. Reads `options` so it re-runs on any toggle.
  $effect(() => {
    const next = options;
    if (!instance) return;
    instance.setOptions(next);
    instance.rerender();
  });
</script>

<div class="diff">
  <div class="diff-head">
    <span class="glyph" aria-hidden="true">±</span>
    <span class="path">{$diffPath}</span>
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
  {:else if parsed.isEmpty || !fileDiff}
    <div class="diff-empty"><p>No textual changes.</p></div>
  {/if}

  <!-- The FileDiff renders into this element via shadow DOM; it stays mounted so
       a single instance can re-render across diff changes. Hidden when there's
       nothing renderable so only the fallback message above shows. -->
  <diffs-container
    bind:this={container}
    class="diffs"
    class:hidden={!fileDiff}
  ></diffs-container>
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

  /* The @pierre/diffs container. The --diffs-* custom properties inherit across
     the shadow boundary, so setting them here bridges Pierre's chrome to the
     app's terminal tokens (which Center.svelte already overrides per-mode via
     terminalSurfaceOverride). Token (syntax) colors come from the pierre-light/
     pierre-dark Shiki themes; only the chrome is overridden. */
  .diffs {
    display: block;
    /* Match the previous diff view's monospace styling. */
    --diffs-font-family: var(--mono);
    --diffs-font-size: var(--r1);
    /* Base surface → the terminal panel fill. */
    --diffs-light-bg: var(--term-bg2);
    --diffs-dark-bg: var(--term-bg2);
    --diffs-bg-context-override: var(--term-bg2);
    /* Add/del row tints → derived from the app's diff accent tokens. */
    --diffs-bg-addition-override: oklch(from var(--diff-add) l c h / 0.1);
    --diffs-bg-deletion-override: oklch(from var(--diff-del) l c h / 0.1);
    --diffs-bg-addition-emphasis-override: oklch(from var(--diff-add) l c h / 0.22);
    --diffs-bg-deletion-emphasis-override: oklch(from var(--diff-del) l c h / 0.22);
    --diffs-addition-color-override: var(--diff-add);
    --diffs-deletion-color-override: var(--diff-del);
    /* Line-number gutter + hover → dim terminal tokens. */
    --diffs-fg-number-override: var(--term-dim);
    --diffs-bg-hover-override: oklch(from var(--term-fg) l c h / 0.06);
  }
  .diffs.hidden {
    display: none;
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
