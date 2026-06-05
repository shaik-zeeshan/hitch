# Structure — IA and layout

Layout spec for the Paper Terminal shell. Values are extracted verbatim from
[mockup.html](mockup.html). See [colors.md](colors.md) for every token referenced
here and [components.md](components.md) for the CSS recipes.

## The macOS window

Hidden-titlebar style (Tauri `titleBarStyle: "Overlay"` / transparent titlebar).
The app paints its own top bar; the OS traffic lights sit inside it.

- Window frame in the mockup: `1440 x 900`, `border-radius: 11px`,
  `1px solid var(--line)` border, `overflow: hidden`, `box-shadow: var(--win-shadow)`.
  In production the window is the real OS window; the radius/shadow/border are the
  mockup's page seat and are not part of the shell. The internal layout below is
  what ships.
- Root vertical stack: top bar (fixed) over the body grid (fills remaining height).

```
+--------------------------------------------------------------+  <- window
| topbar  42px                                                 |
+----------+--------------------------------+------------------+
| rail-l   |  center                        | rail-r           |
| 295px    |  1fr                           | 330px            |
|          |                                |                  |
|          |                                |                  |
+----------+--------------------------------+------------------+
```

## Top bar

One bar, height `42px`, `flex: 0 0 42px`. Background
`linear-gradient(var(--paper-2), var(--paper-1))`, bottom `1px solid var(--line)`.
The whole bar is the drag region (`-webkit-app-region: drag`); every interactive
child sets `no-drag`. `padding: 0 16px`.

There is **no app name, no breadcrumb, and no git status** in the bar. Three
zones only:

```
[● ● ●]                  [ search  Jump to worktree…   ⌘K ]                 [☀]  ● daemon connected
 lights                       palette (centered)                        toggle    daemon
 left                     absolutely centered to window                       right cluster
```

| Element | Placement | Notes |
| --- | --- | --- |
| Traffic lights | Left, `gap: 8px`, `no-drag` | Three `12px` circles. In production these are the OS lights; in the mockup they are painted (`.lights i`). |
| Command palette trigger | `position: absolute; left: 50%; translate: -50% -50%` — centered to the **window width**, not the flex remainder | `300px` wide (`max-width: 42vw`), `26px` tall. Search glyph + placeholder + right-aligned `⌘K` keycap. Placeholder text: `Jump to worktree, session, or action…`. Opens the command palette. |
| Theme toggle | Right cluster | `28x28` square instrument, sun in light / moon in dark. |
| Daemon indicator | Right cluster, standalone | `dot + "daemon" + "connected"`. A standalone quiet instrument: no shared border/background with the toggle or palette. `margin-left: 4px`, comfortable `gap`. The dot is `--st-ok` with a soft glow ring. |

The right cluster order left-to-right: theme toggle, then daemon. A `flex: 1`
spacer (`.grow`) pushes them right.

## Body grid

```css
.body { display: grid; grid-template-columns: 295px 1fr 330px; min-height: 0; }
```

| Column | Width | Role |
| --- | --- | --- |
| Left rail | `295px` | Projects tree + worktrees. Wide enough for full branch names. |
| Center | `1fr` | Tab strip over the edge-to-edge terminal. |
| Right rail | `330px` | Changes: branch context, git action, staged/unstaged files. |

## The 38px header baseline grid

Three column headers share one horizontal baseline so the columns read as a
single aligned system. Each is `flex: 0 0 38px; height: 38px` with a bottom
`1px solid var(--line)` hairline meeting the same y:

| Column | Header element | Content |
| --- | --- | --- |
| Left | `.rail-head` | `PROJECTS` label (left) + `+` add button (right). |
| Center | `.tabs` (the tab strip) | The session tabs, filling the full 38px height. |
| Right | `.changes-head` | `CHANGES` label (left) + `+N −N` net diffstat (right). |

The tab strip's bottom hairline uses `--term-line` (the terminal's own hairline)
rather than `--line`, because the active tab's ink bridges across it. The left
and right header hairlines use `--line`. All three sit at the same y.

## Left rail — Projects

`background: var(--paper-1)`, `border-right: 1px solid var(--line)`, vertical
flex: header (38px) / scrolling tree (`flex: 1`) / footer.

### Header

`PROJECTS` label: `0.6875rem`, `letter-spacing: 0.14em`, uppercase, weight 700,
`--ink-2`. Add button `.add`: `20x20` square, hairline border, `--paper-2` fill,
`+` glyph.

### Project row

`.proj > .row`: mono `0.8125rem`, weight 600, `--ink-0`. `gap: 7px`,
`padding: 6px 8px`. Contents in order:

1. Twisty `▾`/`▸` (`.tw`, `0.625rem`, `--ink-3`).
2. **Folder icon on every project row** (`.folder`, `15px`, `--ink-2`). Git
   projects and plain folders use the *same* folder icon; they differ only by
   the trailing kind label, never by icon.
3. Project name.
4. Right-aligned trailing element (`margin-left: auto`): either a `.pkind`
   label (`git` or `folder`, `0.625rem`, `--ink-3`) on an expanded/plain
   project, **or** a rolled-up state pill on a collapsed project that has
   attention items (see below).

### Worktree entry (one or two rows)

Worktrees hang under their project in a `<ul class="wt">` with a hairline tree
spine: `margin-left: 8px; border-left: 1px solid var(--line-soft)`.

Each entry `.wrow` is a grid of up to 2 rows; the facepile spans both rows on
the right. **A row with nothing to report (idle, clean, no PR) renders `l1`
only — a single-line entry.**

```
grid-template-columns: 1fr auto;
grid-template-areas: "name pile"
                     "meta pile";
```

```
  ⑂ feature/fast-reload                          (o)(c)(c)   <- l1: branch icon + name | pile
    AWAITING · +18 −4 · ⑃#142                                <- l2: [state word] · diffstat · PR chip | pile
```

- **Row 1 (`.l1`):** a **git-branch icon** (`.branchic`, `12px`, `--ink-3`)
  marking branch identity, then the full branch name (`.name`, mono `0.8125rem`
  weight 600, truncates with ellipsis). There is **no state glyph column**
  (2026-06-05: rows are word-only; state lives on `l2`).
- **Row 2 (`.l2`, present only when there is something to show):**
  `padding-left: 16px` to align under the name. Mono `0.625rem`, tabular
  numerals, `--ink-2`. Sequence: optional **state word** (`.statetag`, uppercase
  `0.5625rem` weight 700, colored by state — rendered **only** for
  `AWAITING` / `ERROR` / `WORKING`, see the state vocabulary) · diffstat (`+N`
  add / `−N` del, colored, weight 600) · PR chip (git-pull-request icon +
  `#142`). `·` separators are `--ink-3`. An idle row with changes shows just
  the diffstat; an error row's meta carries the agent-reported reason (e.g.
  `rate limited`). `ERROR` is a *turn* failure reported by the agent's own
  failure hook — the agent process is still alive at its prompt. A session whose
  process exits never shows `ERROR`: exit is modeled as absence (the session
  closes; ADR 0011). The older `exit 1` sample in the state-gallery mockups
  predates this decision.
- **Facepile (`.pile`):** right-spanning, vertically centered. Overlapping
  **17px circles**, `−8px` margin overlap, each ringed with `box-shadow: 0 0 0
  1.75px var(--pile-ring)` (ring color = the rail background, so circles punch
  out cleanly; on a selected row the ring switches to `--iris-wash`). **One
  circle per live Agent session only** (Claude/Codex), its Session mark inside —
  shell sessions contribute no circle, so a worktree running only shells shows
  the empty pile (2026-06-05). Empty pile collapses to a `4px` spacer.

### Selected worktree

`.wrow.sel`: `background: var(--iris-wash)`, `box-shadow: inset 0 0 0 1px
var(--iris-line)`. Name, meta, branch icon, and PR icon all shift to
`--iris-ink`; separators to `--iris-line`.

### Rolled-up state pill (collapsed project)

On a collapsed project that has attention items, `.row` shows a `.rollup` pill
instead of the `.pkind` label: state glyph + count + word, e.g. `◆ 1 awaiting
input`. Styled in the relevant state's wash + line + ink (mockup shows the
`--st-need` family). Square, hairline, `0.625rem` weight 600.

Membership and mixed states (2026-06-05): an "attention item" is a session in
an **act state** (`needs-approval` or `error`) — `WORKING`/idle never raise the
pill. With mixed act states, **one pill** shows the highest-priority state and
its count (`needs-approval > error`), e.g. 2 awaiting + 1 error → `◆ 2 awaiting
input`; the error surfaces on expansion or once the approvals are handled (it
holds — it never expires unseen). Same derivation as the row word and needdot:
group the act-state sessions by rollup priority, render the top group.

### Footer

`.rail-foot`: top hairline, `linear-gradient(var(--paper-1), var(--paper-3))`,
mono `0.625rem`, `--ink-2`. A **counts line only**, e.g.
`4 sessions · 3 worktrees active` (counts emphasized via `.k`, `--ink-1` weight
600). There is **no daemon-ownership line** here; the daemon indicator lives in
the top bar.

## Center — terminal workbench

`background: var(--paper-3)`, vertical flex: tab strip (38px) over the terminal.

### Tab strip

`.tabs`: `flex: 0 0 38px`, `display: flex; align-items: stretch; gap: 0;
padding: 0`. **Zero strip padding** on every side. Tabs fill the strip's full
height and the first tab hugs the left column divider. Bottom hairline is
`--term-line`. Square tabs (`border-radius: 0`).

The **active tab is the terminal surface**: same `--term-bg2` ink fill running
from the strip's top hairline straight down, side hairlines in `--term-line`,
transparent top border, and a `::after` bridge (`bottom: -1px; height: 2px;
background: var(--term-bg2)`) that covers the strip's bottom hairline so the ink
runs unbroken into the panel below. No chrome is visible above the tabs.

Tab anatomy (`gap: 8px`, mono `0.8125rem`, `padding: 0 14px`):

```
[harness-icon]  name   [state-dot]   ✕
```

1. Harness mark (`.tabmark`, `14px`) — Claude / Codex / shell. A diff tab uses a
   `±` glyph instead.
2. Session name (e.g. `claude`, `shell`, or a filename for a diff tab).
3. State dot (`.needdot`, `6px` circle) — present iff the session's Agent State
   is an act state (`needs-approval` or `error`), both in the same `--st-need`
   oxide (2026-06-05); ringed in `--term-bg2` so it reads on the active ink.
4. Close `✕` (`.closer`, `0.7rem`, `--ink-3`; `--term-dim` on the active tab).

A trailing `+` new-tab affordance (`.newtab`) sits after the last tab.

There is **no workspace-info strip** between the tabs and the terminal (the old
`claude-code · ~/path · branch` line was removed). Scrollback starts directly
under the active tab.

### Terminal panel

`.terminal`: runs **edge-to-edge** — zero gutter, meets the left/right column
dividers and the window bottom directly. `border: none; border-radius: 0`.
Background `linear-gradient(var(--term-bg2), var(--term-bg))`. Body
(`.term-body`) is `padding: 14px 16px 4px`, mono `0.8125rem`, `line-height: 1.65`,
`--term-fg`, scrolls. The `14px` top inset gives the first line breathing room.

The permission prompt is a terminal-internal boxed UI; see
[components.md](components.md#permission-prompt-block).

### Diff view (`DiffTab`)

When a diff tab is active, the center pane shows a **syntax-highlighted unified
diff** instead of a terminal (`apps/desktop/src/lib/components/DiffTab.svelte`).
A `DiffTab` keeps its own header bar (`±` glyph + path + `+N −N` counts) and
renders the diff body through [@pierre/diffs](components.md#diff-view-difftab)
into a shadow-DOM `<diffs-container>` custom element: unified layout, word-level
intra-line emphasis, line-info hunk separators, Shiki-highlighted tokens. The
diff chrome (backgrounds, gutters, add/del tints, fonts) is bridged across the
shadow boundary so the diff follows the **per-mode terminal theme** the same way
the terminal surface does. Full recipe and the token bridge are in
[components.md](components.md#diff-view-difftab); colors in
[colors.md](colors.md#syntax-highlighted-diff--pierrediffs).

## Right rail — Changes

`background: var(--paper-1)`, `border-left: 1px solid var(--line)`, vertical
flex.

### Header (`.changes-head`, 38px)

`CHANGES` label (left, same style as PROJECTS) + net diffstat (right, `.net`,
mono tabular, `+N` add / `−N` del colored).

### Branch context block (`.changes-ctx`)

Directly under the header, its own bottom hairline. Two rows:

- **Branch row (`.branchline`):** git-branch icon (`14px`) + branch name
  (weight 600) + `from {base}` (`--ink-2`) + **right-aligned ahead instrument**
  (`.ahead`, `margin-left: auto`, mono weight 600 tabular, `--st-ok`): `↑3`
  meaning N commits ahead of origin.
- **PR row (`.pr`):** git-pull-request icon + open dot + `open` + `PR #142` +
  optional title fragment. Square chip, hairline, `--paper-2` fill.

### Git action (`.actions`) — the dynamic split button

The single most important interactive element. It mirrors the real component
`apps/desktop/src/lib/components/RightRail.svelte`. **One state machine** derives
both the split button's primary label/action and the enabled state of every
dropdown item. The primary always does the *next* meaningful step.

**Primary-action ladder** (first applicable wins; from RightRail.svelte):

| Condition (in order) | Primary label | Mutates |
| --- | --- | --- |
| Has changes + auto-commit-push on | `Commit & Push` | yes |
| Has changes + auto off | `Commit…` | yes |
| `behind > 0` | `Pull ↓N` | yes |
| `ahead > 0` | `Push ↑N` | yes |
| Not on default branch, has worktree, no open PR | `Create PR` | yes |
| Open PR exists | `Open PR #N` | no |
| Otherwise (clean + synced on default) | `Up to date` (disabled) | no |

When a long-running git op is in flight, the whole split is replaced by one
destructive `Cancel` button.

Layout: split button (`.splitbtn` = `.split-main` primary segment +
`.split-caret` 30px caret segment) on the iris fill. Below it, the
**"why this action" hint line** (`.why-primary`, faint mono `0.625rem`,
`--ink-3`) explains why the primary is primary, e.g. `2 staged · ↑3 · PR #142
open`. A quiet **Stash** text button sits at the end of that line
(`margin-left: auto`, `--ink-2`, `Stash ⌘S`).

**Caret menu (`.act-menu`)** — crisp letterpress popover, no blur, square,
hidden by default (revealed with the `actions` hash for review). Items, in
order, with the current primary marked `.is-primary` and inapplicable steps
shown disabled with a `title` reason:

```
  ◷ Commit…                      (commit icon)
  ◷ Commit & Push        ⌘↵       (is-primary in this state)
  ────────────
  ↑ Push                 ↑N       (disabled if ahead == 0 → "Nothing to push")
  ↓ Pull                 ↓N       (disabled if behind == 0 → "Up to date with remote")
  ────────────
  ⑃ Open PR #142         ↗        (shown when an open PR exists)
  + Create PR…                    (shown otherwise; disabled on default branch /
                                   no worktree, with reason)
  ────────────
  ☑ auto-generate commit message  (toggle, menuitemcheckbox; does not close menu)
```

### File list (`.files`)

Two groups, each `.fgroup` with an `.fgroup h3` head: group name + count + a
hairline rule + a right-aligned bulk action (`unstage all` for Staged,
`stage all` for Changes; the real component also offers `Discard` on the
unstaged group).

- **Staged** — files already staged.
- **Changes** — unstaged working-tree changes.

Each `.frow` (mono `0.8125rem`): stage checkbox (`.chk`, 14px square; filled
iris with `✓` when on) + status letter (`.st`, see vocabulary) + **file-type
icon** (`.ftype`, a full-colour VS Code Material Icons glyph, `16px` slot,
rendered as `<img>` — see below) + path (dir dimmed `--ink-2`, filename
`--ink-0`) + right-aligned per-file diffstat (`+N −N`, or `new` for an added
file). The active/selected row gets a `--paper-3` fill + inset hairline. In
production, hovering a row reveals inline stage/unstage (`+`/`−`) and discard
(`×`) affordances.

The **file-type icon** sits between the status letter and the path in both the
staged and unstaged rows. It is a full-colour
[VS Code Material Icons](icons.md#file-type-icons-vs-code-material-icons) glyph,
rendered as `<img src>` at a `16px` slot — a deliberate colour exception in the
otherwise monochrome shell, chosen for instant recognition at this small size
(it is *not* tinted to an ink token). The row is `align-items: center`, so the
fixed `16px` box stays vertically centred without growing the row. The resolver
(`apps/desktop/src/lib/file-icons.ts`, `fileIconUrl(path)`) delegates precedence
to the library: exact file name → compound suffix → extension → language →
generic `"file"` glyph; it never throws (see
[icons.md](icons.md#file-type-icons-vs-code-material-icons)).

### Footer (`.rail-r-foot`)

Keyboard legend, mono `0.625rem`: `␣ stage · ↵ open diff · ⌘↵ commit`.

## Keyboard surface

| Key | Action | Where |
| --- | --- | --- |
| `⌘K` | Open command palette / jump | Top bar palette |
| `⌘↵` | Commit (or the current primary action) | Right rail action; file footer |
| `⌘S` | Stash | Right rail hint line |
| `␣` | Stage / unstage selected file | File list |
| `↵` | Open diff for selected file | File list |

The mockup's permission-prompt keys (`y`/`n`/`a`) are **not Hitch bindings**:
the prompt is the agent's own TUI inside the PTY and accepts whatever keys the
agent defines (see the status note on the permission block in
[components.md](components.md#permission-prompt-block)). Hitch's shell never
intercepts keys destined for the terminal.

Keycaps render with the one shared `kbd` recipe (see
[components.md](components.md#keycap-kbd)).

## State vocabulary

State on worktree rows is conveyed by **word + color** — the colored uppercase
state word is itself the non-color channel, so "state is never color alone"
still holds. The word appears **only when the user must act or be informed**;
anything else is simply idle and carries no label. (Decided 2026-06-05;
stalled was removed — a session that is not working is just idle, there is no
duration heuristic.)

| State | Word | Token | When shown | Meaning |
| --- | --- | --- | --- | --- |
| Awaiting | `AWAITING` | `--st-need` (oxide red) | always (act) | Blocked on human input/approval. The unmissable signal — the only saturated text in a quiet rail. |
| Error | `ERROR` | `--st-need` family | always (act) | Agent-reported turn failure (API error: rate limit, billing, server…); the meta carries the reason (e.g. `rate limited`). Never a process exit — exited sessions close and clear. |
| Working | `WORKING` | `--st-run` (teal-ink) | while streaming (informs) | Agent is actively producing output. |
| Idle / clean | — none | — | never | Not working = nothing to label. Diffstat / PR chip / facepile are the only meta; with nothing to report the row is a single line. |

Accepted trade-off: "finished and idle" and "hung mid-task" look the same;
awaiting/error cover the cases that need attention. When `WORKING` flips back
to idle is an output-activity threshold chosen at implementation time (display
concern only, not a user-facing state).

The rolled-up project pill and the tab state dot keep their marks (the pill's
`◆`, the `6px` needdot); the daemon dot uses `--st-ok` when connected.
`--st-stall` (ochre) survives only as the `M` file-status letter color.
