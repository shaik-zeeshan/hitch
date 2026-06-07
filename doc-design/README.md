# Hitch shell design — "Paper Terminal"

Canonical design documentation for the Hitch desktop shell (macOS Tauri + Svelte
app that supervises AI coding agents across git worktrees).

Direction: **Paper Terminal**. Approved 2026-06-05. The implementation will be
built later from these docs.

## Design thesis

A warm letterpress-paper chrome wraps a deep-ink terminal. The chrome is quiet,
exact, and trustworthy: typeset labels, hairline rules, tabular numerals, and a
strictly rectangular shape language, so it reads like a lab notebook rather than
a dashboard. The terminal panel is the darkest surface in both themes, the
unmistakable figure against the paper ground. The terminal is the product; the
chrome exists to route attention to it and to make the next git action obvious.
Nothing animates that does not convey a state change.

## File map

| File | Contents |
| --- | --- |
| [structure.md](structure.md) | Information architecture and layout: window, top bar, 3-column grid, 38px header baseline, left/center/right rail specs, keyboard surface, state vocabulary. |
| [colors.md](colors.md) | Full OKLCH token tables for both themes, semantic state palette, contrast and usage rules. |
| [icons.md](icons.md) | Icon inventory with production sources and alternatives. Grounded in what the app already has installed. |
| [components.md](components.md) | Per-component build specs with exact CSS recipes (tabs, worktree entry, facepile, split button, keycap, palette, PR chip, state tags, permission prompt, file rows, motion). |
| [mockup.html](mockup.html) | The single-file approved mockup. Source of truth for every value. |
| mockup-light.png | Screenshot, light "paper" theme. |
| mockup-dark.png | Screenshot, dark "dusk" theme. |
| mockup-actions.png | Screenshot, split-button action menu revealed. |
| mockup-worktree-states.html / .png | Worktree-row state gallery: the four-state model (word-only rows, state word only for awaiting/error/working), selected variants, edge cases. |
| mockup-worktree-glyphs.html / .png | Historical exploration: unicode vs drawn state glyphs + row-anatomy options. Superseded by the word-only decision; kept for the record. |

## How to view the mockup

Open `mockup.html` in any browser. It is self-contained (no build, fonts load
from Google Fonts).

- **Theme toggle:** click the sun/moon instrument at the top right of the bar.
- **Force a theme on load via the URL hash:** `mockup.html#light` or
  `mockup.html#dark`. Default with no hash is light.
- **Reveal the git action menu** (the split-button popover) for review: add
  `actions` to the hash, e.g. `mockup.html#actions` or `mockup.html#dark-actions`.

The hash logic lives in the inline `<script>` at the bottom of the file: any
hash containing `dark` selects the dark theme, any hash containing `actions`
adds `show-actions` to `<html>` which un-hides the popover.

## Cross-references

Build order for an implementer: read [structure.md](structure.md) for the IA,
then [colors.md](colors.md) to install the tokens, then [components.md](components.md)
for the recipes, pulling icon sources from [icons.md](icons.md) as each
component needs them.
