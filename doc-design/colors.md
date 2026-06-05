# Colors — token tables

Every custom property from [mockup.html](mockup.html), both themes, with usage
notes. All values are OKLCH. See [structure.md](structure.md) for where each
token is consumed and [components.md](components.md) for recipes.

## Rules

1. **OKLCH only.** Never `#000` or `#fff`, never a hard pure black or white,
   anywhere. The lightest paper is `~97% L` and the darkest ink is `~22-28% L`.
2. **Warm-tinted neutrals.** Paper and ink neutrals carry a small warm chroma
   (hue family ~70-82). The dark theme keeps the same warm hue family rather than
   going cold/grey.
3. **Accent is reserved.** The iris accent appears only for **selection**,
   **the primary action**, and **state**. It is never decorative.
4. **The terminal follows the theme but stays cool.** In light it is a
   paper-light surface; in dusk it is the darkest surface (lowest L). What
   separates the terminal from the chrome in *both* themes is hue: the
   terminal is cool (blue, ~262-265) against the warm paper/ink neutrals.
   (Earlier revisions kept the terminal dark in both themes; superseded —
   each theme now themes the terminal to match.)
5. **State is never color alone** (see the state vocabulary in
   [structure.md](structure.md#state-vocabulary)); state renders as a colored
   uppercase **word** — the word is the non-color channel. State words appear
   only when the user must act or be informed (`AWAITING` / `ERROR` /
   `WORKING`); idle/clean is unlabelled.

## Neutrals — paper and ink

| Token | Light (`:root`) | Dark (`[data-theme="dark"]`) | Usage |
| --- | --- | --- | --- |
| `--paper-0` | `oklch(97.4% 0.008 80)` | `oklch(16.5% 0.012 72)` | Page behind window; body backdrop base. |
| `--paper-1` | `oklch(96.6% 0.010 78)` | `oklch(19.0% 0.013 72)` | Rails (left + right), facepile ring color. |
| `--paper-2` | `oklch(98.0% 0.008 82)` | `oklch(22.0% 0.014 74)` | Raised cards, inputs, popover, instrument fills. |
| `--paper-3` | `oklch(94.0% 0.012 76)` | `oklch(17.5% 0.012 70)` | Sunken wells, tab strip bg, hover fill, center bg. |
| `--line` | `oklch(88.0% 0.014 74)` | `oklch(31.0% 0.016 74)` | Hairlines, borders, dividers. |
| `--line-soft` | `oklch(91.5% 0.012 76)` | `oklch(26.0% 0.014 72)` | Softer hairlines (tree spine). |
| `--ink-0` | `oklch(28.0% 0.020 70)` | `oklch(91.0% 0.014 82)` | Primary text. |
| `--ink-1` | `oklch(42.0% 0.018 70)` | `oklch(78.0% 0.014 80)` | Secondary text, emphasized counts. |
| `--ink-2` | `oklch(56.0% 0.016 72)` | `oklch(64.0% 0.014 78)` | Tertiary text, labels, icons. |
| `--ink-3` | `oklch(66.0% 0.014 74)` | `oklch(52.0% 0.013 76)` | Faint text, separators, twisties. |

## Terminal panel

The cool-tinted surface. Follows the theme: paper-light in light, deep ink in
dusk; the cool hue (~262-265) is what separates it from the warm chrome.

| Token | Light | Dark | Usage |
| --- | --- | --- | --- |
| `--term-bg` | `oklch(96.8% 0.006 265)` | `oklch(12.0% 0.018 262)` | Terminal base (gradient bottom). |
| `--term-bg2` | `oklch(98.4% 0.005 265)` | `oklch(13.8% 0.018 262)` | Terminal top of gradient; active-tab fill; bridge. |
| `--term-line` | `oklch(87.0% 0.014 265)` | `oklch(26.0% 0.022 262)` | Terminal/tab hairlines (the seam). |
| `--term-fg` | `oklch(27.0% 0.020 265)` | `oklch(90.0% 0.012 90)` | Terminal foreground text. |
| `--term-dim` | `oklch(54.0% 0.016 265)` | `oklch(64.0% 0.016 100)` | Dim terminal text, active-tab close glyph. |

In-terminal ANSI-ish accents (literal values, not tokenized) — one set per
theme, since the surface flips with the theme.

Dark (tuned for the deep-ink surface): `t-grn oklch(82% 0.13 150)`, `t-red
oklch(78% 0.13 28)`, `t-cy oklch(82% 0.10 195)`, `t-yl oklch(86% 0.12 92)`,
`t-iris oklch(80% 0.10 280)`, `t-b oklch(96% 0.01 90)`; ANSI black maps to
`--term-line` so it stays visible; cursor `oklch(82% 0.10 92)`; selection
`oklch(60% 0.10 280 / 0.32)`.

Light (darker inks tuned for the paper surface): `t-grn oklch(52% 0.13 150)`,
`t-red oklch(52% 0.16 28)`, `t-cy oklch(52% 0.10 195)`, `t-yl oklch(58% 0.11
92)`, `t-iris oklch(48% 0.13 280)`, `t-b oklch(45% 0.012 90)` (ANSI white maps
to a mid grey ink so it stays visible on paper); ANSI black `oklch(30% 0.02
265)`; cursor `oklch(48% 0.11 92)`; selection `oklch(50% 0.12 280 / 0.22)`.

## Iris accent

Selection + primary action partner.

| Token | Light | Dark | Usage |
| --- | --- | --- | --- |
| `--iris` | `oklch(48.0% 0.150 275)` | `oklch(64.0% 0.150 278)` | Accent fill (primary button, checkboxes on). |
| `--iris-ink` | `oklch(40.0% 0.160 275)` | `oklch(80.0% 0.120 280)` | Accent text/icon on paper; selected-row ink; button border. |
| `--iris-wash` | `oklch(93.5% 0.030 275)` | `oklch(28.0% 0.060 278)` | Selected-row background; facepile ring when selected. |
| `--iris-line` | `oklch(82.0% 0.060 275)` | `oklch(42.0% 0.090 278)` | Selected-row inset border; iris separators. |
| `--iris-on` | `oklch(98% 0.01 280)` | `oklch(18% 0.03 280)` | Text/icon on the iris fill. |
| `--iris-on-sc` | `oklch(92% 0.04 280)` | `oklch(28% 0.06 280)` | Secondary glyph on iris fill. |
| `--iris-on-sc-line` | `oklch(60% 0.10 280)` | `oklch(56% 0.10 280)` | Hairline of a keycap drawn on the iris fill; caret divider. |

## Semantic state palette

State renders as a colored word (see
[structure.md](structure.md#state-vocabulary)) — shown only for awaiting /
error / working; idle has no state color. Light values are darkened for AA
on paper; dark values are lifted for AA on dusk.

| Token | Light | Dark | State |
| --- | --- | --- | --- |
| `--st-run` | `oklch(48.0% 0.090 195)` | `oklch(74.0% 0.120 195)` | Working / streaming (teal-ink). |
| `--st-need` | `oklch(47.0% 0.150 32)` | `oklch(72.0% 0.150 32)` | Awaiting approval / error (oxide red). |
| `--st-need-wash` | `oklch(94.0% 0.035 32)` | `oklch(28.0% 0.060 32)` | Wash behind rollup pill / awaiting chrome. |
| `--st-need-line` | `oklch(83.0% 0.080 32)` | `oklch(42.0% 0.090 32)` | Border on the awaiting chrome. |
| `--st-stall` | `oklch(52.0% 0.075 75)` | `oklch(78.0% 0.110 78)` | Ochre — only the `M` (modified) status letter. The stalled state was removed (2026-06-05). |
| `--st-ok` | `oklch(50.0% 0.095 150)` | `oklch(74.0% 0.120 152)` | Positive/ok (green-ink): daemon dot, PR open dot, `↑N` ahead instrument, `A` added letter. Clean is no longer a labelled state. |
| `--st-ok-glow` | `oklch(50% 0.095 150 / .14)` | `oklch(74% 0.120 152 / .18)` | Soft glow ring on the daemon dot / ok dots. |

## Diff numerals

| Token | Light | Dark | Usage |
| --- | --- | --- | --- |
| `--diff-add` | `oklch(46.0% 0.095 150)` | `oklch(76.0% 0.120 152)` | `+N` additions. |
| `--diff-del` | `oklch(50.0% 0.130 28)` | `oklch(74.0% 0.140 28)` | `−N` deletions. |

## Harness marks

Two layers: the ringed-circle facepile fills (`--harness-*`) and the mark
inks (`--mark-*`) tuned for AA against those fills. The facepile in the mockup
fills the circle with `--paper-2` and colors the mark with `--mark-*`; the
`--harness-*` family is provided for any solid-fill rendering.

| Token | Light | Dark | Usage |
| --- | --- | --- | --- |
| `--harness-claude` | `oklch(58% 0.150 30)` | `oklch(62% 0.150 30)` | Claude solid fill (coral). |
| `--harness-codex` | `oklch(46% 0.040 250)` | `oklch(60% 0.060 250)` | Codex solid fill (neutral blue). |
| `--harness-shell` | `oklch(44% 0.020 80)` | `oklch(58% 0.022 80)` | Shell solid fill (warm neutral). |
| `--harness-fg` | `oklch(98% 0.01 90)` | `oklch(14% 0.012 80)` | Mark color on a solid harness fill. |
| `--mark-claude` | `oklch(63% 0.165 32)` | `oklch(72% 0.155 34)` | Claude sunburst ink on the ringed circle. |
| `--mark-codex` | `oklch(40% 0.030 250)` | `oklch(82% 0.030 250)` | Codex blossom ink. |
| `--mark-shell` | `oklch(40% 0.018 80)` | `oklch(82% 0.018 80)` | Shell terminal-mark ink. |

## Page seat and shadows

Mockup-only chrome (the simulated window seat); not part of the shipped shell
but reproduced here for completeness.

| Token | Light | Dark |
| --- | --- | --- |
| `--page-glow` | `oklch(98.4% 0.010 82)` | `oklch(20.0% 0.016 74)` |
| `--light-inset` | `oklch(100% 0 0 / .6)` | `oklch(100% 0 0 / .04)` |
| `--win-shadow` | layered: `0 1px 0 oklch(100% 0 0 / .6) inset, 0 28px 60px -28px oklch(30% 0.03 70 / .35), 0 6px 18px -10px oklch(30% 0.03 70 / .22)` | `0 0 0 1px oklch(10% 0.01 72 / .5), 0 24px 60px -32px oklch(4% 0.01 72 / .7)` |

Traffic-light fills (mockup-only): red `oklch(63% 0.17 25)`, yellow
`oklch(78% 0.14 85)`, green `oklch(72% 0.15 145)`.

## Non-color design tokens

Declared in `:root` and used across components:

| Token | Value | Note |
| --- | --- | --- |
| `--radius` | `6px` | Defined but the shell is rectangular; see the radius policy in [components.md](components.md#shape--radius-policy). |
| `--mono` | `"JetBrains Mono", ui-monospace, "SF Mono", Menlo, monospace` | Mono stack. JetBrains Mono is loaded from Google Fonts (weights 400/500/600/700 + italic 400). |
| `--ui` | `-apple-system, BlinkMacSystemFont, "SF Pro Text", "Segoe UI", system-ui, sans-serif` | UI sans stack. |
| `--r0`..`--r4` | `.75 / .8125 / .875 / 1 / 1.125 rem` | Type scale (12 / 13 / 14 / 16 / 18 px at 16px root). |

## Contrast / AA notes

- Body text `--ink-0` on `--paper-1` clears AA comfortably in both themes
  (light ~28% on ~96.6% L; dark ~91% on ~19% L).
- Label/secondary `--ink-2` on paper is the floor for small text; it is used
  for labels and meta only, at weight 600-700, which keeps it legible. Do not
  use `--ink-3` for anything that must be read (separators and twisties only).
- State colors are split per theme precisely so each clears AA on its own
  background: light state hues are pushed darker (lower L), dark state hues are
  lifted. Never reuse a light state value on a dark surface or vice-versa.
- The iris primary button uses `--iris-on` (near-white in light, dark ink in
  dark) on the `--iris` fill — both AA.
- Inside the terminal, the permission prompt uses its own literal oklch values
  tuned for AA on the deep ink (greens for approve, oxide for deny); see
  [components.md](components.md#permission-prompt-block).
