# Frontend stack: Svelte + Vite + Tailwind v4

The `apps/desktop` frontend is built on **Svelte** (Svelte 5, runes) + **Vite** + **Tailwind CSS v4**, not React. This revises the frontend choice noted in passing in [0005](0005-monorepo-crate-structure.md); everything else in 0005 — the crate DAG and `src-tauri` as a thin proto-only IPC client — stands unchanged.

## Stack

- **Svelte 5** with `@sveltejs/vite-plugin-svelte` and `svelte-check` for type checking. Plain Svelte + Vite (an SPA mounted in the Tauri webview), **not SvelteKit** — there is no server, routing, or SSR to justify it.
- **Tailwind CSS v4** via `@tailwindcss/vite`, OKLCH-native `@theme`. Design tokens are ported verbatim from the locked mockup (`hitch-shell-mockup.html`).
- **`bits-ui` v2** for headless overlays (dialog, dropdown, popover, context-menu, command palette). Its `Command` and `ContextMenu` primitives cover the ⌘K palette and the worktree/session menus — no `cmdk-sv` needed.
- **A local unified-diff classifier** (`lib/diff.ts`) for the diff tab, not `@pierre/diffs`. The locked mockup renders a flat, un-highlighted diff (hunk/add/del/ctx rows with a line-number gutter); `@pierre/diffs`'s `FileDiff` is a heavyweight imperative Shiki/worker renderer whose output deviates from the lock. The classifier matches the locked design at no Shiki/worker cost. `@pierre/diffs` stays in `package.json` (tree-shaken out of the bundle) should a richer syntax-highlighted view ever be wanted.
- **`@xterm/xterm` + `@xterm/addon-fit`** retained from the React scaffold for the terminal.
- Package manager **bun**; Vite dev server stays port 1420 `strictPort`, ignoring `src-tauri`.

## Titlebar

macOS window uses `titleBarStyle: "Overlay"` + `hiddenTitle: true` so the traffic lights sit inside the unified dark top nav the mockup draws, rather than a separate native bar. The app reserves left padding for the lights.

## Considered Options

- **Keep React** (the provisional scaffold) — rejected: the team is standardizing on Svelte; one framework for all future UI work.
- **SvelteKit** — rejected: a Tauri webview needs an SPA, not a meta-framework; plain Svelte + Vite is the smaller, more direct fit.
- **Native title bar** — rejected: breaks the single unified top nav in the locked design.

## Consequences

- The React build config and entry are replaced; `react`, `react-dom`, `@vitejs/plugin-react`, and `@types/react*` are dropped. The daemon IPC contract (`connect_daemon`, `hitch_request`, `send_session_input`, the kebab-case request `type` strings) is unchanged — it is ported, not redesigned.
- No `src-tauri` source change; only `tauri.conf.json` (titlebar) is touched there. The Rust side stayed otherwise frozen with one approved exception: `hitch-git`'s `diff_file` was emitting libgit2 line *content* without the per-line origin marker, so `FileDiff.diff` was not a valid unified diff and the diff tab could not classify add/del lines. A ~4-line fix re-prepends the `+`/`-`/` ` origin; `cargo build` and `hitch-git`'s tests still pass.
- Canonical docs that named React ([0005](0005-monorepo-crate-structure.md), `CONTEXT.md`) are corrected to say Svelte.
