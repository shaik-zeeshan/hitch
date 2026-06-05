# Icons — inventory and sources

The mockup uses hand-drawn inline `<svg><symbol>` marks referenced with `<use>`
(no external requests). An implementer needs real, maintainable sources. This
file gives, per slot: purpose, the mockup's approach, a recommended production
source, and alternatives.

## What the app already has

Checked `apps/desktop/package.json` and `apps/desktop/src`:

- **No icon library is installed.** No `lucide`, `@lucide/svelte`,
  `@primer/octicons`, `simple-icons`, `@iconify`, or `feather` in dependencies
  or `node_modules`.
- Every icon in the existing components (`RightRail.svelte`, `TopNav.svelte`,
  `SessionTabs.svelte`, `ProjectTree.svelte`, etc.) is a **bespoke inline SVG**,
  stroke-style, `viewBox="0 0 16 16"`, `stroke-width` ~1.3-1.6, `currentColor`.
- The UI primitive library is **`bits-ui`** (used for the dropdown menu); it is
  not an icon set.

## Recommendation

**Adopt [unplugin-icons](https://github.com/unplugin/unplugin-icons) with
[Iconify](https://iconify.design) collections** (decided 2026-06-05). Icons are
imported as Svelte components from the `~icons/<collection>/<name>` virtual
module and compiled to inline SVG at build time — tree-shaken, no runtime, no
external requests.

- **Generic UI icons: the Lucide collection** (`@iconify-json/lucide`,
  `~icons/lucide/git-branch`). Lucide matches the existing hand-rolled house
  style (1.5px stroke, round caps/joins, 24px grid, `currentColor`) and covers
  every generic slot below.
- **Harness marks (Claude, Codex, shell): a custom local collection** via
  unplugin-icons' `FileSystemIconLoader` (e.g. `~icons/hitch/claude`), using
  the exact SVG paths reproduced below. They are brand/identity marks, not
  generic UI icons. Iconify's `simple-icons` collection
  (`~icons/simple-icons/anthropic`, `~icons/simple-icons/openai`) is the
  alternative where brand licensing clearly permits.

Setup in `apps/desktop/vite.config.ts` (SvelteKit + Vite):

```ts
import Icons from "unplugin-icons/vite";
import { FileSystemIconLoader } from "unplugin-icons/loaders";

plugins: [
  tailwindcss(),
  sveltekit(),
  Icons({
    compiler: "svelte",
    customCollections: {
      // harness marks: one .svg file per mark in src/lib/icons/
      hitch: FileSystemIconLoader("./src/lib/icons"),
    },
  }),
],
```

Install `unplugin-icons` + `@iconify-json/lucide` (per-collection JSON, not the
full `@iconify/json`). For TypeScript, reference `unplugin-icons/types/svelte`
in `app.d.ts` or tsconfig `types`.

Usage: `import GitBranch from "~icons/lucide/git-branch"` then
`<GitBranch class="icon" />`.

**Stroke-width caveat:** Iconify's Lucide bodies bake `stroke-width="2"` as a
presentation attribute. The mockup wants **1.5px** — override it in CSS (CSS
wins over presentation attributes), e.g. a shared icon class:
`.icon, .icon * { stroke-width: 1.5; }`. Do not mix a second generic icon
collection; anything added must be stroke-consistent with Lucide.

Stroke conventions to enforce everywhere: **1.5px stroke, round caps and joins,
`currentColor`, 12-15px rendered size, no fills except the harness marks and
status dots.**

## Inventory

### Harness marks (identity — keep custom)

| Slot | Mockup approach | Production source | Alternatives |
| --- | --- | --- | --- |
| **Claude** | Filled tapered-ray sunburst (`#mk-claude`), `--mark-claude` coral. | Custom collection: `~icons/hitch/claude` (sunburst path below as `claude.svg`). | `~icons/simple-icons/anthropic` / `~icons/simple-icons/claude` if brand licensing permits. |
| **Codex / OpenAI** | Outline blossom/knot, six rotated petal strokes (`#mk-codex`). | Custom collection: `~icons/hitch/codex` (blossom path below). | `~icons/simple-icons/openai` (the official OpenAI knot). |
| **Shell** | Rounded-square terminal with a prompt chevron + line (`#mk-shell`). | Custom collection: `~icons/hitch/shell` (no canonical brand). | `~icons/lucide/square-terminal` or `~icons/lucide/terminal`. |

### Generic UI icons (Lucide collection via unplugin-icons)

Import as `~icons/lucide/<name>`.

| Slot | Purpose | Mockup approach | Lucide name | Octicon / other alt |
| --- | --- | --- | --- | --- |
| Folder | Every project row | Outline folder (`#ic-folder`) | `folder` | Octicon `file-directory` |
| Git branch | Worktree branch identity + Changes branch row | Two-circle fork (`#ic-branch`) | `git-branch` | Octicon `git-branch` (`@primer/octicons`) |
| Git pull-request | PR chips | Circle + arrow-line-circle (`#ic-pr`) | `git-pull-request` | Octicon `git-pull-request` |
| Git commit | Commit action | Circle on a line (`#ic-commit`) | `git-commit` (`git-commit-horizontal`) | Octicon `git-commit` |
| Commit & push | Primary action icon | Commit node feeding an up-arrow (`#ic-commitpush`) | Compose: `git-commit` + `arrow-up`, or just `arrow-up-from-line` | custom (keep) |
| Push | Menu item | Up arrow (`#ic-push`) | `arrow-up` | — |
| Pull | Menu item | Down arrow (`#ic-pull`) | `arrow-down` | — |
| Search | Palette glyph | Circle + handle | `search` | — |
| Sun | Theme toggle (light active) | Disc + 8 rays | `sun` | — |
| Moon | Theme toggle (dark active) | Crescent | `moon` | — |
| Close | Tab close, file discard | `✕` text glyph | `x` | — |
| Caret down | Split-button caret, twisty | `M6 9 l6 6 l6-6` | `chevron-down` | — |
| Plus | Add project, create PR | `+` text glyph / cross stroke | `plus` | — |
| External link | Open PR `↗` | `↗` text glyph | `arrow-up-right` / `external-link` | — |

### Pure-CSS / glyph marks (no icon needed)

| Slot | How |
| --- | --- |
| Daemon dot | CSS circle, `--st-ok` + `--st-ok-glow` glow ring. |
| Tab state dot (`.needdot`) | CSS circle, `--st-need`, ringed in `--term-bg2`. |
| State words (`AWAITING`/`ERROR`/`WORKING`) | Colored uppercase mono statetags — words, not icons. Worktree rows have no state glyph column (2026-06-05). |
| Status letters `M`/`A`/`D`/`U` | Mono text glyphs colored per status (M ochre, A green, D red, U faint). |
| PR open dot | CSS circle, `--st-ok`. |
| Checkbox tick | `✓` text glyph inside the box. |

## Custom SVG sources (exact paths from the mockup)

Ship the harness marks as standalone `.svg` files in `src/lib/icons/`
(`claude.svg`, `codex.svg`, `shell.svg`) for the `hitch` custom collection;
the generic shapes below are reference fallbacks should a Lucide slot ever
need replacing. All `viewBox="0 0 24 24"`.

**Claude — `#mk-claude`** (filled, `fill="currentColor"`):

```
M12 2.2 L13 11 L12 12 L11 11 Z
M21.8 12 L13 13 L12 12 L13 11 Z
M12 21.8 L11 13 L12 12 L13 13 Z
M2.2 12 L11 11 L12 12 L11 13 Z
M18.9 5.1 L12.9 11.1 L12 12 L11.95 10.6 Z
M18.9 18.9 L13.4 12.9 L12 12 L13.4 11.05 Z
M5.1 18.9 L11.1 12.9 L12 12 L12.05 13.4 Z
M5.1 5.1 L10.6 11.1 L12 12 L10.6 12.95 Z
```

**Codex — `#mk-codex`** (`fill="none" stroke="currentColor" stroke-width="2"`,
round caps/joins). One petal stroke `M12 5.2 a5.6 5.6 0 0 1 5.6 5.6` repeated at
`rotate(0|60|120|180|240|300 12 12)`.

**Shell — `#mk-shell`** (`fill="none" stroke="currentColor" stroke-width="1.8"`,
round caps/joins):

```
rect x=3.2 y=3.2 w=17.6 h=17.6 rx=4.4
M8 9 l3 3 l-3 3
M13 15 h3.2
```

**Folder — `#ic-folder`** (`stroke-width="1.5"`):

```
M3 6.6 a1.8 1.8 0 0 1 1.8-1.8 h4.2 l1.8 2.2 h7.4 a1.8 1.8 0 0 1 1.8 1.8 v8.6
  a1.8 1.8 0 0 1-1.8 1.8 H4.8 A1.8 1.8 0 0 1 3 17.4 Z
```

**Git branch — `#ic-branch`** (`stroke-width="1.5"`): circles `(6.5,5 r2.2)`,
`(6.5,19 r2.2)`, `(17.5,7 r2.2)`; paths `M6.5 7.2 v9.6` and
`M17.5 9.2 v1.3 a4.5 4.5 0 0 1-4.5 4.5 H10`.

**Git pull-request — `#ic-pr`** (`stroke-width="1.5"`): circles `(6.5,6 r2.2)`,
`(6.5,18.5 r2.2)`, `(17.5,18.5 r2.2)`; paths `M6.5 8.2 v8.1` and
`M17.5 16.3 V9.4 l-2.6 2.6 M17.5 9.4 l2.6 2.6` (translated `0 -0.2`).

**Git commit — `#ic-commit`** (`stroke-width="1.5"`): circle `(12,12 r3.4)`,
paths `M3 12 h5.6 M15.4 12 H21`.

**Commit & push — `#ic-commitpush`** (`stroke-width="1.6"`): circle
`(7,12 r3)`, paths `M10.2 12 H15`, `M18 18 V7 M14.6 10.4 18 7 21.4 10.4`.

**Push — `#ic-push`** (`stroke-width="1.7"`): `M12 19 V6 M6.5 11.5 12 6 17.5 11.5`.

**Pull — `#ic-pull`** (`stroke-width="1.7"`): `M12 5 V18 M6.5 12.5 12 18 17.5 12.5`.

**Search (palette)** (`stroke-width="1.7"`): `circle (10.5,10.5 r6.2)`,
`M15.2 15.2 20 20`.

**Sun** (`stroke-width="1.6"`): `circle (12,12 r4.2)` plus the 8-ray path
`M12 2.4v2.6M12 19v2.6M4.4 4.4l1.9 1.9M17.7 17.7l1.9 1.9M2.4 12h2.6M19 12h2.6M4.4 19.6l1.9-1.9M17.7 6.3l1.9-1.9`.

**Moon** (`stroke-width="1.6"`): `M20 14.2A8 8 0 0 1 9.8 4 8 8 0 1 0 20 14.2z`.

**Caret down** (split button / menu) (`stroke-width="2"`): `M6 9 l6 6 l6-6`.
