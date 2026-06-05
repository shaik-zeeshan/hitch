# Components — build specs

Per-component recipes extracted from [mockup.html](mockup.html). Selectors are
paraphrased; **values are exact**. Token names resolve in
[colors.md](colors.md). Placement and behavior are in
[structure.md](structure.md). Icon sources are in [icons.md](icons.md).

## Shape / radius policy

The shell is **rectangular**. `border-radius: 0` on every button, chip, tag,
input, popover, keycap, checkbox, split button, action menu, PR chip, rollup
pill, and theme toggle. The crisp letterpress edge is intentional.

**Only true circles survive** `border-radius: 50%`: facepile session marks,
the daemon dot, traffic lights, tab state dots, the small state/ok dots. The
mockup window frame itself uses `11px` (page seat only, not the shell). A
`--radius: 6px` token exists but is unused by the shell.

## Motion rules

- Transitions are `150-250ms ease-out` and exist **only to convey a state
  change** (hover, theme swap, selection). No ambient/decorative motion.
- Concrete transitions in the mockup: palette border `.18s ease-out`; theme
  toggle `color/border-color/background .18s ease-out`.
- The only keyframe animation is the terminal cursor blink
  (`@keyframes blink 1.1s steps(1) infinite`).
- **`prefers-reduced-motion: reduce`** disables the cursor blink and the theme
  toggle transition. Honor it for every transition/animation added later.

## Keycap (`kbd`)

One recipe everywhere. Flat rectangle, hairline border, **no bevel, no shadow**,
tabular mono in a fixed box, baseline-nudged to sit with its label. The box is
wider than tall (15×20 minimum) so a lone glyph reads as a keycap, not a tile.

```css
kbd {
  display: inline-flex; align-items: center; justify-content: center;
  height: 15px; min-width: 20px; padding: 0 5px;
  font-family: var(--mono); font-size: 10.5px; font-weight: 600;
  font-variant-numeric: tabular-nums; letter-spacing: .02em; line-height: 1;
  color: var(--ink-2); background: transparent;
  border: 1px solid var(--line); border-radius: 0;
  vertical-align: baseline; translate: 0 1px;
}
```

**Multi-key shortcuts render one keycap per key** — never two glyphs crammed
into a single cap. `⌘K` is `[⌘][K]`: each key in its own `kbd`, grouped by a
`.keys` row.

```css
.keys { display: inline-flex; align-items: center; gap: 3px; }
```

Variants:
- **On the iris fill** (split button primary): `color: var(--iris-on);
  border-color: var(--iris-on-sc-line)` — inverts to a light glyph on a
  translucent hairline.
- **Inside the dark PTY permission choices**: borderless-ink caps tuned per
  choice for AA (green for approve, oxide for deny, dim for always-allow). See
  the permission block.

## Command palette field (`.palette`)

```css
.palette {
  position: absolute; left: 50%; top: 50%; translate: -50% -50%;
  display: inline-flex; align-items: center; gap: 8px;
  width: 300px; max-width: 42vw; height: 26px; padding: 0 8px 0 9px;
  background: var(--paper-2); border: 1px solid var(--line); border-radius: 0;
  color: var(--ink-3); cursor: text;
  transition: border-color .18s ease-out;
}
.palette:hover { border-color: var(--ink-3); }
```

Children: `.seek` search glyph `13px` `--ink-3`; `.ph` placeholder (`--r0`,
`--ink-3`, single line, ellipsis) reading `Jump to worktree, session, or
action…`; a right-aligned `[⌘][K]` keycap pair (`.keys`). Absolutely centered
to the window, not the flex remainder.

## Theme toggle (`.theme-toggle`)

```css
.theme-toggle {
  width: 28px; height: 28px; display: grid; place-items: center;
  border: 1px solid var(--line); border-radius: 0;
  background: var(--paper-2); color: var(--ink-2);
  transition: color .18s ease-out, border-color .18s ease-out, background .18s ease-out;
}
.theme-toggle:hover { color: var(--ink-1); border-color: var(--ink-3); }
.theme-toggle svg { width: 15px; height: 15px; }
```

Sun shown in light, moon shown in dark (toggle visibility on
`html[data-theme="dark"]`).

## Daemon indicator (`.daemon`)

Standalone, no grouping border/background. Mono `--r0`, `--ink-2`.
`gap: 7px`. Dot: `7px` circle, `background: var(--st-ok)`, `box-shadow: 0 0 0
3px var(--st-ok-glow)`. The status word (`connected`) is `--ink-1` weight 600.

## Tabs

```css
.tabs {
  flex: 0 0 38px; height: 38px; display: flex; align-items: stretch; gap: 0;
  padding: 0; background: var(--paper-3);
  border-bottom: 1px solid var(--term-line);
  position: relative; z-index: 2;
}
.tab {
  display: inline-flex; align-items: center; gap: 8px;
  font-family: var(--mono); font-size: var(--r1); color: var(--ink-2);
  padding: 0 14px; border: 1px solid transparent; border-bottom: none;
  border-radius: 0; position: relative;
}
.tab.active {
  background: linear-gradient(var(--term-bg2), var(--term-bg2));
  color: var(--term-fg); font-weight: 600;
  border-color: var(--term-line); border-top-color: transparent;
}
.tab.active::after {  /* bridge over the strip's bottom hairline */
  content: ""; position: absolute; left: 0; right: 0; bottom: -1px; height: 2px;
  background: var(--term-bg2); z-index: 3;
}
```

Details: Session mark `.tabmark` `14px` (Claude coral, shell `--ink-2`/`--term-dim`
when active, diff `±` glyph). `.needdot` `6px` circle `--st-need` ringed
`box-shadow: 0 0 0 3px var(--term-bg2)` — shown iff the session's Agent State is
an **act state** (`needs-approval` or `error`; 2026-06-05). Both act states use
the same oxide dot: one color, one meaning — "act here". Never shown for
working/waiting. `.closer` `✕` `0.7rem` `--ink-3`
(`--term-dim` active). `.newtab` `+` trailing affordance, `--ink-3`. Zero strip
padding; first tab hugs the left divider.

## Terminal panel

```css
.terminal {
  flex: 1; min-height: 0; display: flex; flex-direction: column;
  background: linear-gradient(var(--term-bg2), var(--term-bg));
  border: none; border-radius: 0; overflow: hidden;
}
.term-body {
  flex: 1; min-height: 0; overflow: auto; padding: 14px 16px 4px;
  font-family: var(--mono); font-size: var(--r1); line-height: 1.65;
  color: var(--term-fg);
}
```

Edge-to-edge: no gutter, meets column dividers + window bottom. Cursor:
`.cursor` `8px` x `1.05em` block, `background: oklch(82% 0.10 92)`, blink
animation (disabled under reduced-motion).

## Diff view (`DiffTab`)

When a diff tab is active the center pane swaps the terminal for a
**syntax-highlighted unified diff** (`apps/desktop/src/lib/components/DiffTab.svelte`).
This supersedes the earlier flat, un-highlighted classified-row view (2026-06-06;
ADR 0006 holds the prior decision as the historical record — left intact).

- **Header bar** (kept by `DiffTab`, *not* @pierre's): `±` glyph + the file path
  + a right-aligned `+N −N` count. `disableFileHeader: true` suppresses
  @pierre/diffs' own header so this one is the only one shown. The `+N`/`−N`
  counts and binary/empty detection come from `apps/desktop/src/lib/diff.ts`,
  which now survives only for that (its line classifier is no longer rendered).
- **Body** is rendered by **[@pierre/diffs](https://www.npmjs.com/package/@pierre/diffs)**
  (vanilla core, not React; Shiki with the `shiki-js` engine — no WASM). DiffTab
  parses the libgit2 unified-diff string with `processFile(str, { isGitDiff: true })`
  and renders through the `FileDiff` class into a shadow-DOM `<diffs-container>`
  custom element. Options:

  | Option | Value | Effect |
  | --- | --- | --- |
  | `diffStyle` | `'unified'` | One-column unified diff (not split). |
  | `lineDiffType` | `'word'` | Word-level intra-line emphasis. |
  | `diffIndicators` | `'classic'` | `+`/`−` gutter indicators. |
  | `hunkSeparators` | `'line-info'` | `@@`-style line-info hunk separators. |
  | `disableFileHeader` | `true` | DiffTab keeps its own header bar (above). |

- **Token colors:** Shiki themes `pierre-light` / `pierre-dark`, with `themeType`
  following the app theme store.
- **Chrome token bridge:** panel/gutter backgrounds, add/del row tints, and the
  mono font are bridged across the shadow boundary via `--diffs-*-override` CSS
  custom properties set **inline** on `<diffs-container>`. They inherit across the
  shadow DOM and resolve against the app's `--term-bg` / `--term-bg2` /
  `--term-line` / `--term-fg`, `--diff-add` / `--diff-del`, and `--mono`. This is
  the same `terminalSurfaceOverride` pattern the terminal uses, so the diff
  **follows the per-mode terminal theme** in both themes. See
  [colors.md](colors.md#syntax-highlighted-diff--pierrediffs).

## Worktree entry (`.wrow`)

```css
.wt { margin-left: 8px; border-left: 1px solid var(--line-soft); }  /* tree spine */
.wt .wrow {
  display: grid; grid-template-columns: 1fr auto;
  grid-template-areas: "name pile" "meta pile";
  align-items: center; column-gap: 4px;
  min-width: 0; padding: 5px 6px 5px 7px; margin: 1px 0; border-radius: 0;
  font-family: var(--mono);
}
.wt .wrow .l1 { grid-area: name; display: flex; align-items: center; gap: 5px; min-width: 0; }
.wt .wrow .branchic { width: 12px; height: 12px; color: var(--ink-3); flex: 0 0 12px; margin-right: -1px; }
.wt .wrow .name {
  font-size: var(--r1); font-weight: 600; color: var(--ink-0);
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis; min-width: 0;
}
.wt .wrow .l2 {
  grid-area: meta; display: flex; align-items: center; gap: 4px;
  margin-top: 2px; padding-left: 16px; /* aligns under the name (no glyph column) */
  font-size: .625rem; color: var(--ink-2); font-variant-numeric: tabular-nums;
  white-space: nowrap; overflow: hidden; text-overflow: ellipsis;
}
```

There is **no state glyph column** — rows are word-only (2026-06-05) and start
at the branch icon. `.l2` is rendered **only when there is something to show**;
an idle, clean row with no PR is a single-line entry (`l1` + pile only).

Meta children: optional `.statetag` (uppercase `0.5625rem` weight 700,
`letter-spacing: .04em`, colored by state class — present **only** for
`AWAITING` / `ERROR` / `WORKING`; idle rows carry no state word); `.sep` `·`
`--ink-3`; `.diffn .a/.d` colored `--diff-add`/`--diff-del` weight 600;
`.prchip` (pr icon `12px` + `#N`, weight 600, state-color keyed like the right
rail's PR chip: `.open`→`--st-ok`, `.merged`→`--pr-merged`,
`.closed`→`--st-need`, `.draft`→`--ink-3`; the state word lives in the `title`
tooltip — that word, not color alone, is the non-color channel).

**Selected (`.wrow.sel`):** `background: var(--iris-wash); box-shadow: inset 0 0
0 1px var(--iris-line)`. Name/meta/branchic → `--iris-ink`; sep →
`--iris-line`; pile ring → `--iris-wash`. The PR chip keeps its state color
under selection (same rule as statetags); only the faint `.draft` chip lifts to
`--iris-ink` so it stays legible on the wash.

Statetag classes: `.statetag.need`→`--st-need` (awaiting **and** error),
`.statetag.run`→`--st-run` (working). There are no glyph classes and no
stalled/ok tags — idle and clean are unlabelled.

## Facepile (`.pile`)

```css
.pile { grid-area: pile; display: flex; align-items: center; flex: 0 0 auto; }
.pile.empty { width: 4px; }
.pile .h {
  width: 17px; height: 17px; border-radius: 50%;
  display: grid; place-items: center; line-height: 1;
  box-shadow: 0 0 0 1.75px var(--pile-ring, var(--paper-1));
  margin-left: -8px;
  background: var(--paper-2);
}
.pile .h:first-child { margin-left: 0; }
.pile .h svg { width: 12px; height: 12px; }      /* claude 12; codex/shell 13 */
```

Mark color per Session mark: `.h.claude`→`--mark-claude`,
`.h.codex`→`--mark-codex`. The pile holds **Agent sessions only** (2026-06-05):
shell sessions draw no circle, so `.h.shell`/`--mark-shell` survive only for the
tab strip's session marks, never in the pile. Selected row sets `--pile-ring:
var(--iris-wash)` so overlaps read against the wash.

## Rollup pill (`.rollup`)

Collapsed-project attention pill. `margin-left: auto`, inline-flex, `gap: 5px`,
mono `0.625rem` weight 600. `color: var(--st-need)`, `background:
var(--st-need-wash)`, `border: 1px solid var(--st-need-line)`, `border-radius:
0`, `padding: 1px 7px 1px 6px`. Leading `.g` glyph `0.7rem`.

## Split button + popover menu

```css
.splitbtn {
  display: flex; align-items: stretch; width: 100%;
  border-radius: 0; overflow: hidden;
  box-shadow: 0 1px 0 oklch(100% 0 0 / .14) inset;
}
.split-main {
  flex: 1; min-width: 0; justify-content: center;
  font-family: var(--ui); font-size: var(--r1); font-weight: 600;
  display: inline-flex; align-items: center; gap: 8px;
  padding: 8px 12px;
  background: var(--iris); color: var(--iris-on);
  border: 1px solid var(--iris-ink); border-right: none;
}
.split-main .btnic { width: 14px; height: 14px; flex: 0 0 14px; color: var(--iris-on); }
.split-caret {
  flex: 0 0 30px; display: grid; place-items: center;
  background: var(--iris); color: var(--iris-on);
  border: 1px solid var(--iris-ink);
  box-shadow: inset 1px 0 0 var(--iris-on-sc-line);
}
.split-caret svg { width: 13px; height: 13px; }
```

The label, icon, and `kbd` are state-derived (see the action ladder in
[structure.md](structure.md#git-action-actions--the-dynamic-split-button)). In
production this is `bits-ui` `DropdownMenu` (already used in
`RightRail.svelte`): the caret is the `DropdownMenu.Trigger`. When a git op is
running, the whole split becomes one `Cancel` button.

**Why-primary hint line (`.why-primary`):** `margin-top: 8px`, mono `0.625rem`,
`--ink-3`, tabular. Holds the reason (`2 staged · ↑3 · PR #142 open`) and a
right-aligned quiet **Stash** text button (`margin-left: auto`, `--ink-2` →
`--ink-1` on hover, `Stash ⌘S`).

**Action menu (`.act-menu`):** crisp letterpress popover, **no blur**.

```css
.act-menu {
  position: absolute; right: 16px; top: calc(100% - 6px); z-index: 20;
  min-width: 230px; padding: 5px;
  background: var(--paper-2); border: 1px solid var(--line); border-radius: 0;
  box-shadow: 0 1px 0 var(--light-inset) inset,
              0 14px 30px -14px oklch(30% 0.03 70 / .35),
              0 4px 10px -6px oklch(30% 0.03 70 / .25);
}
.mi { display: flex; align-items: center; gap: 9px; padding: 7px 9px; border-radius: 0;
      font-family: var(--ui); font-size: var(--r1); color: var(--ink-0); }
.mi:hover { background: var(--paper-3); }
.mi .mi-ico { width: 15px; height: 15px; flex: 0 0 15px; color: var(--ink-2); }
.mi .mi-k { margin-left: auto; font-family: var(--mono); font-size: .625rem; color: var(--ink-2); }
.mi.is-primary { color: var(--iris-ink); font-weight: 600; }
.mi.is-primary .mi-ico { color: var(--iris-ink); }
.mi.disabled { opacity: .42; }
.m-sep { height: 1px; background: var(--line); margin: 5px 7px; }
```

Dark theme swaps the shadow for a ring (`0 0 0 1px oklch(8% 0.01 72 / .5), 0 16px
34px -16px oklch(4% 0.01 72 / .7)`). Mockup reveal: `html.show-actions .act-menu
{ display: block }`.

**Auto-generate toggle (`.mi.toggle`):** `--ink-1`, leading `.check` box
(`15px` square, hairline, `border-radius: 0`); `.check.on` fills `--iris` with
the `✓` in `--iris-on`; off-state hides the tick (`color: transparent`). Does
not close the menu on select.

## PR chip (`.pr`)

The whole chip is washed in the PR-state color (2026-06-05; the mockup's
in-chip `open` word + dot were dropped in favor of the tint). The state word
lives in the `title` tooltip — that word is the non-color channel.

```css
.pr {
  display: inline-flex; align-items: center; gap: 7px; margin-top: 9px;
  font-family: var(--mono); font-size: var(--r0); color: var(--ink-1);
  background: var(--paper-2); border: 1px solid var(--line); border-radius: 0;
  padding: 3px 10px 3px 8px; text-decoration: none;
}
.pr:hover .num { text-decoration: underline; }
.pr .num { font-weight: 600; }
.pr .pric { width: 13px; height: 13px; color: currentColor; }
/* state tint: ink + wash + line per state; draft stays faint paper */
.pr.open   { color: var(--st-ok);    background: var(--st-ok-wash);    border-color: var(--st-ok-line); }
.pr.merged { color: var(--pr-merged); background: var(--pr-merged-wash); border-color: var(--pr-merged-line); }
.pr.closed { color: var(--st-need);  background: var(--st-need-wash);  border-color: var(--st-need-line); }
.pr.draft  { color: var(--ink-2); }
```

## State tags

Uppercase mono, `0.5625rem`, weight 700, `letter-spacing: .04em`, colored by the
state token (`.need` for awaiting/error, `.run` for working). The colored word
is itself the non-color channel — never color alone. Rendered **only when the
user must act or be informed** (`AWAITING` / `ERROR` / `WORKING`); idle and
clean rows carry no state tag (see the state vocabulary in
[structure.md](structure.md#state-vocabulary)).

## Permission prompt block

Terminal-internal UI rendered as a boxed listing inside the PTY scrollback.

> **Status (2026-06-05): illustrative, not a Hitch component.** The permission
> prompt is the agent's own TUI inside the PTY; Hitch cannot restyle those bytes
> and does not render or answer prompts (the hook helper is one-way reporting,
> never a decision channel — CONTEXT.md "Hook helper"). This recipe depicts the
> *ideal* look for mockup purposes only. The real prompt is whatever the agent
> draws (Claude Code: number/arrow-driven choices, not `y`/`n`/`a`). Hitch's
> contribution to this moment is the `AWAITING` state word, tab needdot, and
> rollup pill that route the user here.

```css
.perm {
  margin: 10px 0 6px; border: 1px solid oklch(43% 0.095 32); border-radius: 0;
  overflow: hidden; background: oklch(22% 0.030 32);
}
/* dark: border oklch(40% 0.09 32); background oklch(18% 0.030 32) */
.perm .ph {  /* header */
  padding: 7px 12px; font-family: var(--mono); font-size: .6875rem;
  color: oklch(84% 0.11 32); letter-spacing: .08em; font-weight: 700;
  border-bottom: 1px solid oklch(37% 0.075 32);
  display: flex; align-items: center; gap: 8px;
}
.perm .pb { padding: 11px 14px; font-family: var(--mono); font-size: var(--r1); color: var(--term-fg); line-height: 1.7; }
.perm .cmd { color: oklch(86% 0.12 92); font-weight: 600; }   /* the command */
.perm .why { color: var(--term-dim); }                         /* inline # comment */
.perm .choices { display: flex; gap: 10px; margin-top: 9px; padding-top: 10px;
  border-top: 1px dashed oklch(40% 0.055 32); }
.choice { display: inline-flex; align-items: center; gap: 8px;
  font-family: var(--mono); font-size: .8125rem; padding: 5px 11px; border-radius: 0; }
.choice.yes { background: oklch(39% 0.10 150); color: oklch(94% 0.055 150); }
.choice.no  { background: oklch(31% 0.04 32); color: oklch(85% 0.03 40); border: 1px solid oklch(43% 0.06 32); }
```

Header: glyph `◆` + `PERMISSION REQUIRED` (oxide). Body shows the command and a
`cwd · worktree` context line. Three choices with keycaps:

| Choice | Keycap | Style |
| --- | --- | --- |
| `approve & run` | `y` | green fill; cap border `oklch(72% 0.06 150)` glyph `oklch(95% 0.05 150)`. |
| `deny` | `n` | oxide outline; cap border `oklch(54% 0.05 32)` glyph `oklch(88% 0.03 40)`. |
| `always allow {cmd}` | `a` | transparent/dim (`border-color: oklch(40% 0.03 265); color: var(--term-dim)`); cap border `oklch(50% 0.04 264)` glyph `oklch(90% 0.02 90)`. |

## File rows (`.frow`)

```css
.frow { display: flex; align-items: center; gap: 9px; padding: 5px 8px; border-radius: 0;
  font-family: var(--mono); font-size: var(--r1); }
.frow:hover, .frow.active { background: var(--paper-3); }
.frow.active { box-shadow: inset 0 0 0 1px var(--line); }
.frow .chk { width: 14px; height: 14px; border-radius: 0; flex: 0 0 14px;
  border: 1px solid var(--line); display: grid; place-items: center;
  font-size: .6rem; color: var(--paper-2); }
.frow .chk.on { background: var(--iris); border-color: var(--iris-ink); color: var(--iris-on); }
.frow .st { width: 13px; text-align: center; font-weight: 700; font-size: .75rem; flex: 0 0 13px; }
.frow .ftype { width: 16px; height: 16px; flex: 0 0 16px; display: grid; place-items: center; }
.frow .ftype img { width: 16px; height: 16px; display: block; }  /* full-colour Material Icon, not tinted */
.frow .path { color: var(--ink-2); overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
.frow .path b { color: var(--ink-0); font-weight: 500; }
.frow .fdiff { margin-left: auto; font-size: .625rem; font-variant-numeric: tabular-nums; }
```

Status letters (mono, weight 700, colored — never color alone): `M` modified
`--st-stall` (ochre — the token survives only for this letter; the stalled
state was removed); `A` added `--st-ok` (green); `D` deleted `--diff-del`
(red); `U` untracked/unknown `--ink-3` (faint). (Letter→color map from the live
`RightRail.svelte`.) Diffstat: `.a` add `.d` del; an added file shows `new`.

**File-type icon (`.ftype`)** sits between the status letter and the path in both
staged and unstaged rows. It is a **full-colour** VS Code Material Icons glyph
(the `vscode-material-icons` package) rendered as `<img src>` at a `16px` slot —
a deliberate colour exception in the otherwise monochrome shell, chosen for
instant recognition at this small size (it is *not* tinted to an ink token). The
fixed box plus the row's `align-items: center` keeps it vertically centred without
growing the row. The resolver lives in `apps/desktop/src/lib/file-icons.ts`
(`fileIconUrl(path)`); see
[icons.md](icons.md#file-type-icons-vs-code-material-icons) for its precedence,
fallback, and asset strategy.

Group head (`.fgroup h3`): mono `0.625rem` uppercase `letter-spacing: .1em`
weight 700 `--ink-2`; count `.ct` `--ink-3`; a `.hr` flex hairline; a right-aligned
bulk action `.all` (`--iris-ink`, e.g. `stage all` / `unstage all`).

## Footers

- **Left rail (`.rail-foot`):** top hairline, `linear-gradient(var(--paper-1),
  var(--paper-3))`, mono `0.625rem` `--ink-2`, `line-height: 1.6`. Emphasis via
  `.k` (`--ink-1` weight 600). Counts only.
- **Right rail (`.rail-r-foot`):** top hairline, mono `0.625rem` `--ink-2`,
  inline keyboard legend with keycaps.

## Scrollbars

Not custom-styled in the mockup; the tree (`.tree`), terminal body
(`.term-body`), and file list (`.files`) use `overflow: auto` with default
scrollbars. If styled later, keep them thin, neutral (`--ink-3` thumb on
transparent track), and consistent across both themes.

## Inline-icon helper (`.svgi`)

Shared wrapper for inline marks: `display: inline-block; vertical-align:
-0.18em; color: var(--ink-2); flex: 0 0 auto;` with `svg { width: 100%; height:
100%; display: block; }`. Size is set on the wrapper per use site.
