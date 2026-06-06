// Shared @pierre/diffs renderer wrapper for the diff views. DiffTab (one file
// or a collapsed directory's files) and DiffAllTab (every changed file, each an
// expandable section) render identical per-file @pierre/diffs instances, differing
// only in their data path and surrounding chrome. This module owns the parts that
// are byte-identical between them: the render-side FileDiffOptions, the per-section
// Svelte action that owns a FileDiff instance's lifecycle, and the --diffs-* chrome
// token bridge (applied imperatively by the action so it lives in one place rather
// than being duplicated in each component's scoped <style>).
import { FileDiff, type FileDiffMetadata, type FileDiffOptions } from "@pierre/diffs";
import type { DiffStyle } from "./settings";
import type { Theme } from "./theme";

// Render-side view options for a FileDiff instance. Driven by the persisted
// `diffStyle` / `diffWrap` settings and the app theme — these only re-lay-out an
// already-fetched diff (split vs unified, wrap vs scroll, light vs dark), so a
// change applies via setOptions + rerender on the live instances rather than a
// re-fetch. Both diff views build identical options; the only per-view nuance is
// why `disableFileHeader` is set (DiffTab renders its own header bar; DiffAllTab
// renders a per-section header row) — the value is the same either way.
export function diffViewOptions(
  diffStyle: DiffStyle,
  diffWrap: boolean,
  theme: Theme,
): FileDiffOptions<undefined> {
  return {
    diffStyle,
    disableFileHeader: true, // each view renders its own header / sub-header.
    disableLineNumbers: false,
    diffIndicators: "classic",
    hunkSeparators: "line-info",
    lineDiffType: "word",
    overflow: diffWrap ? "wrap" : "scroll",
    stickyHeader: false,
    preferredHighlighter: "shiki-js", // no WASM, faster startup.
    theme: { light: "pierre-light", dark: "pierre-dark" },
    themeType: theme,
  };
}

// The @pierre/diffs --diffs-* chrome token bridge. The custom properties inherit
// across the shadow boundary, so setting them on the <diffs-container> host bridges
// Pierre's chrome to the app's terminal tokens (which Center.svelte already
// overrides per-mode via terminalSurfaceOverride). Token (syntax) colors come from
// the pierre-light / pierre-dark Shiki themes; only the chrome is overridden here.
// Applied by the action (node.style.setProperty) so this lives in one place rather
// than being duplicated in each component's scoped <style> — Svelte scoping would
// otherwise force a copy of this block per component.
const DIFFS_CHROME_VARS: Record<string, string> = {
  // Match the previous diff view's monospace styling.
  "--diffs-font-family": "var(--mono)",
  "--diffs-font-size": "var(--r1)",
  // Base surface → the terminal panel fill.
  "--diffs-light-bg": "var(--term-bg2)",
  "--diffs-dark-bg": "var(--term-bg2)",
  "--diffs-bg-context-override": "var(--term-bg2)",
  // Add/del row tints → derived from the app's diff accent tokens.
  "--diffs-bg-addition-override": "oklch(from var(--diff-add) l c h / 0.1)",
  "--diffs-bg-deletion-override": "oklch(from var(--diff-del) l c h / 0.1)",
  "--diffs-bg-addition-emphasis-override": "oklch(from var(--diff-add) l c h / 0.22)",
  "--diffs-bg-deletion-emphasis-override": "oklch(from var(--diff-del) l c h / 0.22)",
  "--diffs-addition-color-override": "var(--diff-add)",
  "--diffs-deletion-color-override": "var(--diff-del)",
  // Line-number gutter + hover → dim terminal tokens.
  "--diffs-fg-number-override": "var(--term-dim)",
  "--diffs-bg-hover-override": "oklch(from var(--term-fg) l c h / 0.06)",
};

export type ViewParams = {
  fileDiff: FileDiffMetadata;
  opts: FileDiffOptions<undefined>;
};

// Svelte action: owns one FileDiff instance for the section it's attached to. It
// renders on mount + on metadata/theme/option change and cleans up when the
// section unmounts (cleanUp() detaching the Svelte-owned <diffs-container> is fine
// because Svelte is removing it too). Threading the opts through the action params
// lets a split/wrap/theme toggle re-apply to every live instance via setOptions +
// rerender. Used by both DiffTab (per file section) and DiffAllTab (per expanded
// section); the chrome token bridge is applied here so it lives in one place.
export function fileDiffView(node: HTMLElement, params: ViewParams) {
  // <diffs-container> is a custom element with no default display; the old scoped
  // CSS set `display: block`, so keep that here alongside the chrome token bridge.
  node.style.display = "block";
  for (const [prop, value] of Object.entries(DIFFS_CHROME_VARS)) {
    node.style.setProperty(prop, value);
  }

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
        // New metadata: a full re-render (also picks up the latest opts).
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
