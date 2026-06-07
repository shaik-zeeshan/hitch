# Frontend stack: SvelteKit + Vite + Tailwind v4

The `apps/desktop` frontend is built on **SvelteKit** (Svelte 5, runes, static SPA mode) + **Vite** + **Tailwind CSS v4**, not React. This revises the frontend choice noted in passing in [0005](0005-monorepo-crate-structure.md); everything else in 0005 — the crate DAG and `src-tauri` as a thin proto-only IPC client — stands unchanged.

## Stack

- **SvelteKit** with `@sveltejs/adapter-static`, `@sveltejs/vite-plugin-svelte`, and `svelte-check` for type checking. It runs as a static SPA mounted in the Tauri webview: `adapter({ fallback: "index.html" })` provides the shell for `/` and `/settings`, routes/layouts live under `src/routes`, and there is no SSR/server dependency.
- **Tailwind CSS v4** via `@tailwindcss/vite`, OKLCH-native `@theme`. Design tokens are ported verbatim from the locked mockup (`../../doc-design/mockup.html` from this ADR, or `doc-design/mockup.html` from the repo root).
- **`bits-ui` v2** for headless overlays (dialog, dropdown, popover, context-menu, command palette). Its `Command` and `ContextMenu` primitives cover the ⌘K palette and the worktree/session menus — no `cmdk-sv` needed.
- **A local unified-diff classifier** (`lib/diff.ts`) for the diff tab, not `@pierre/diffs`. The locked mockup renders a flat, un-highlighted diff (hunk/add/del/ctx rows with a line-number gutter); `@pierre/diffs`'s `FileDiff` is a heavyweight imperative Shiki/worker renderer whose output deviates from the lock. The classifier matches the locked design at no Shiki/worker cost. `@pierre/diffs` stays in `package.json` (tree-shaken out of the bundle) should a richer syntax-highlighted view ever be wanted.
- **`@xterm/xterm` + `@xterm/addon-fit`** retained from the React scaffold for the terminal.
- Package manager **bun**; Vite dev server stays port 1420 `strictPort`, ignoring `src-tauri`.

## Titlebar

The unified dark top nav the mockup draws is the title bar on every platform; how the OS chrome folds into it differs.

- **macOS** uses `titleBarStyle: "Overlay"` + `hiddenTitle: true` so the traffic lights sit inside the top nav rather than a separate native bar. The app reserves left padding for the lights (`.topnav.mac`).
- **Windows** has no equivalent title-bar-overlay mode, so the window is frameless (`decorations: false`, set in the platform override `tauri.windows.conf.json`) and the app draws its own caption controls (minimize / maximize / close) into the right of the top nav (`WindowControls.svelte`), using native Win11 metrics (full-height 46px buttons, red close-hover) themed with the app's tokens. Min/close are ordinary HTML buttons calling the Tauri window API. The maximize button keeps **Snap Layouts** working through a transparent native child window (`src-tauri/src/window_chrome.rs`) parked over the button that returns `HTMAXBUTTON` from `WM_NCHITTEST`. Subclassing the *top-level* window proc does **not** work — WebView2 hosts the page in a child HWND that consumes the client-area hit-testing — but a child overlay above it does, while staying visually transparent because WebView2 composites through DWM, not GDI (the technique `tauri-plugin-frame` uses). The overlay forwards its non-client hover/click back as `hitch-max-button-hover` / `hitch-max-button-click` events (highlight + maximize toggle, keeping Tauri's window state in sync), and the frontend reports the button rectangle via `set_max_button_rect` so the overlay tracks it across layout/resize/DPI changes.
- **Linux** keeps the OS's native title bar above the top nav (the macOS-only `titleBarStyle` is ignored there); no custom caption controls.

## Considered Options

- **Keep React** (the provisional scaffold) — rejected: the team is standardizing on Svelte; one framework for all future UI work.
- **Plain Svelte + Vite without SvelteKit routing/layouts** — rejected: the shipped desktop shell uses SvelteKit's file-based routes and layouts while still building to a static SPA for Tauri; this keeps routing structure explicit without introducing SSR/server runtime.
- **Native title bar** — rejected: breaks the single unified top nav in the locked design.

## Consequences

- The React build config and entry are replaced; `react`, `react-dom`, `@vitejs/plugin-react`, and `@types/react*` are dropped. The daemon IPC contract (`connect_daemon`, `hitch_request`, `send_session_input`, the kebab-case request `type` strings) is unchanged — it is ported, not redesigned.
- No `src-tauri` source change; only `tauri.conf.json` (titlebar) is touched there. The Rust side stayed otherwise frozen with one approved exception: `hitch-git`'s `diff_file` was emitting libgit2 line *content* without the per-line origin marker, so `FileDiff.diff` was not a valid unified diff and the diff tab could not classify add/del lines. A ~4-line fix re-prepends the `+`/`-`/` ` origin; `cargo build` and `hitch-git`'s tests still pass.
- Canonical docs that named React ([0005](0005-monorepo-crate-structure.md), `CONTEXT.md`) are corrected to say Svelte.

## Amendment (2026-06-05): the diff tab is syntax-highlighted via `@pierre/diffs`

The "local unified-diff classifier, not `@pierre/diffs`" decision under **Stack**
is **superseded**. The diff tab now renders a syntax-highlighted, word-level diff
through `@pierre/diffs` (v1.2.3), and the Changes list gained full-colour
file-type icons. The flat un-highlighted classifier the locked mockup drew was
matched at the time, but the user wants file-type recognition and *readable*
diffs — syntax color plus intra-line emphasis on what actually changed — which a
classified row-list cannot give. `@pierre/diffs` was already a `package.json`
dependency (kept on exactly this hypothetical), is framework-agnostic at its
core, and exposes a CSS-custom-property surface that lets it obey the existing
token/theme system rather than fight it. The 0006 trade-off — flat-diff
simplicity, no Shiki, no shadow DOM — is consciously given up for legibility.

**What renders.** `DiffTab.svelte` uses the **vanilla core** of `@pierre/diffs`
(not its React subpath). The Rust side is unchanged: `hitch-git` still emits a
libgit2 unified-diff string (`DiffFormat::Patch`), which `processFile(str,
{isGitDiff: true})` parses and the `FileDiff` class renders into a
`<diffs-container>` shadow-DOM custom element. Options: `diffStyle: 'unified'`,
`lineDiffType: 'word'`, `diffIndicators: 'classic'`, `hunkSeparators:
'line-info'`, `disableFileHeader: true` — DiffTab keeps its own header (± path,
+N/−N). Highlighting is Shiki with `preferredHighlighter: 'shiki-js'`, the
pure-JS regex engine, so there is **no WASM blob** in the bundle.

**Theming bridges the shadow boundary.** Token colors come from the
`pierre-light` / `pierre-dark` Shiki themes, with `themeType` driven by the app
theme store. The diff *chrome* is bridged via `--diffs-*-override` custom
properties set inline on the container — CSS custom properties inherit across the
shadow boundary even though rules do not — resolving against the app's
`--term-*` / `--diff-add` / `--diff-del` / `--mono` tokens. The diff therefore
follows the per-mode terminal theme like the rest of the surface, rather than
being a styling island.

**`lib/diff.ts` is retained, narrowed.** It no longer classifies rows for
display; it survives only as the source of the +N/−N counts and the
binary/empty detection that feed DiffTab's own header and fallback states.

## Consequences

- New runtime dependency on `@pierre/diffs` and its Shiki grammar/theme payload,
  now actually in the bundle (no longer tree-shaken out). Shiki bundle weight is
  the cost; choosing the `shiki-js` highlighter avoids the WASM blob but not the
  grammars.
- A shadow-DOM custom element now lives inside the diff tab. The token system
  reaches it only through the `--diffs-*-override` custom-property bridge;
  styling that surface is a second, indirect path next to ordinary Tailwind, and
  a Pierre upgrade could move or rename those override hooks.
- **Changes list file-type icons (same date).** Staged and unstaged rows gained a
  16px file-type icon in a slot between the status glyph and the path, from the
  `vscode-material-icons` npm package (a wrapper around VS Code's Material Icon
  Theme). These are **full-colour, multi-tone** glyphs rendered as `<img src>` —
  a deliberate exception to the otherwise monochrome Paper Terminal palette,
  chosen because a single-ink mark is too hard to recognise at this small size.
  The resolver `src/lib/file-icons.ts` (`fileIconUrl(path)`) delegates precedence
  to the library's `getIconForFilePath` (exact file name → compound suffix →
  extension → language → generic `"file"` fallback) and never throws. The ~900
  SVGs are pulled via `import.meta.glob(..., { query: '?url', eager: true })` and
  emitted as hashed `.svg` assets (not inlined into the JS bundle — `vite.config`
  excludes them from `assetsInlineLimit`). No ADR documented an icon-library
  choice, so this is recorded here rather than as a separate decision; it does
  not disturb the `bits-ui` overlay or `@xterm/xterm` choices above.
