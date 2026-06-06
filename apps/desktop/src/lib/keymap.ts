import { writable } from "svelte/store";
import {
  currentDesktopPlatform,
  shortcutModifierLabel,
  type DesktopPlatform,
} from "./desktopPlatform";

// Centralized, declarative keymap — the single source of truth mapping a
// physical key combo to an app command. Mirrors the terminalKeys.ts pattern: a
// pure, unit-testable matcher (`matchBinding`) with the side effects (command
// dispatch, focus moves) living in the +layout.svelte dispatcher. Slices 2–5
// add command implementations and the Settings reference panel; the binding
// table below is already the WHOLE shortcut set so those slices have nothing to
// invent — they only register handlers for ids that already exist here.
//
// Why a hand-rolled matcher and not a hotkey library: the set is ~20 bindings,
// `<svelte:window>` is the idiomatic Svelte dispatch surface, and a pure
// resolver keeps platform/pane gating verifiable without a DOM (see
// keymap.test.ts). The combo spec is deliberately declarative (a primary
// modifier that resolves to Cmd on macOS / Ctrl elsewhere, plus exact
// shift/alt/ctrl flags) so a future settings tab can render per-platform labels
// and a future override layer can persist rebindings without restructuring.

// The three focusable panes. Bare-key bindings (arrows, Space, Enter, R,
// Backspace) only fire when their `when` matches the focused pane (see
// matchBinding); modifier combos fire regardless of focus.
export type FocusedPane = "tree" | "terminal" | "git";

// Which area a binding belongs to — drives grouping in the future Settings
// reference panel and documents intent here.
export type BindingGroup = "global" | "tabs" | "tree" | "git";

// A combo's modifier requirements. `primary` is the platform shortcut modifier
// (Cmd on macOS, Ctrl elsewhere) resolved at match time. `ctrl` is a LITERAL
// Control requirement on BOTH platforms (used by Ctrl+` and Ctrl+Tab, which are
// Ctrl everywhere — never remapped to Cmd). `shift`/`alt` are literal. Any flag
// left undefined is treated as "must be absent" so matching is exact:
// Cmd+Shift+E never matches a Cmd+E binding, and Cmd+B never matches Cmd+Alt+B.
export interface ComboSpec {
  // The event `key` to match. Compared case-insensitively for letters; literal
  // for named keys ("Escape", "Enter", "Tab", " ", "Backspace", "ArrowUp", …).
  // Digits "1".."9" are literal.
  key: string;
  // Require the platform primary modifier (Cmd/Ctrl).
  primary?: boolean;
  // Require literal Control on every platform (independent of `primary`).
  ctrl?: boolean;
  shift?: boolean;
  alt?: boolean;
}

export interface Binding {
  id: string;
  group: BindingGroup;
  // Human label for the Settings reference panel (slice 5).
  description: string;
  combo: ComboSpec;
  // Pane gate for bare-key bindings. When set, the binding only matches while
  // that pane is focused AND the event target is not an editable element. Omit
  // for modifier combos that should fire from anywhere (the global default).
  when?: FocusedPane;
}

// The full shortcut set (plan §"Shortcut set"). Only the `global` group has
// command handlers wired in slice 1; `tabs`/`tree`/`git` ids exist so later
// slices register handlers against a stable id — the dispatcher ignores any
// binding whose id has no registered handler (it does NOT preventDefault it).
export const bindings: readonly Binding[] = [
  // ---- global -----------------------------------------------------------
  {
    id: "focus.tree",
    group: "global",
    description: "Focus left rail (tree)",
    combo: { key: "e", primary: true, shift: true },
  },
  {
    id: "focus.terminal",
    group: "global",
    description: "Focus terminal",
    // Ctrl+` on BOTH platforms (not the primary modifier).
    combo: { key: "`", ctrl: true },
  },
  {
    id: "focus.git",
    group: "global",
    description: "Focus right rail (git)",
    combo: { key: "g", primary: true, shift: true },
  },
  {
    id: "toggle.left",
    group: "global",
    description: "Toggle left rail",
    combo: { key: "b", primary: true },
  },
  {
    id: "toggle.right",
    group: "global",
    description: "Toggle right rail",
    combo: { key: "b", primary: true, alt: true },
  },
  {
    id: "focus.terminal.escape",
    group: "global",
    description: "Return focus to terminal",
    // Bare Esc, only meaningful when a non-terminal pane is focused; the
    // dispatcher additionally suppresses it while an overlay is open (dialogs
    // own Esc then). Gated to tree/git via the handler, not `when`, because Esc
    // should fire for EITHER pane (a single `when` can't express "tree or git").
    combo: { key: "Escape" },
  },
  {
    id: "palette.open",
    group: "global",
    description: "Open command palette",
    combo: { key: "k", primary: true },
  },
  {
    id: "settings.toggle",
    group: "global",
    description: "Toggle settings",
    combo: { key: ",", primary: true },
  },
  // ---- tabs (handlers land in slice 2) ----------------------------------
  ...(["1", "2", "3", "4", "5", "6", "7", "8", "9"].map((n) => ({
    id: `tab.jump.${n}`,
    group: "tabs" as const,
    description: `Jump to tab ${n}`,
    combo: { key: n, primary: true },
  })) as Binding[]),
  {
    id: "tab.next",
    group: "tabs",
    description: "Next tab",
    combo: { key: "]", primary: true, shift: true },
  },
  {
    id: "tab.prev",
    group: "tabs",
    description: "Previous tab",
    combo: { key: "[", primary: true, shift: true },
  },
  {
    id: "tab.next.ctrl",
    group: "tabs",
    description: "Next tab",
    combo: { key: "Tab", ctrl: true },
  },
  {
    id: "tab.prev.ctrl",
    group: "tabs",
    description: "Previous tab",
    combo: { key: "Tab", ctrl: true, shift: true },
  },
  {
    id: "tab.new",
    group: "tabs",
    description: "New shell tab",
    combo: { key: "t", primary: true },
  },
  {
    id: "tab.close",
    group: "tabs",
    description: "Close active tab",
    combo: { key: "w", primary: true },
  },
  // ---- tree (handlers land in slice 3) ----------------------------------
  {
    id: "tree.up",
    group: "tree",
    description: "Move up",
    combo: { key: "ArrowUp" },
    when: "tree",
  },
  {
    id: "tree.down",
    group: "tree",
    description: "Move down",
    combo: { key: "ArrowDown" },
    when: "tree",
  },
  {
    id: "tree.expand",
    group: "tree",
    description: "Expand project",
    combo: { key: "ArrowRight" },
    when: "tree",
  },
  {
    id: "tree.collapse",
    group: "tree",
    description: "Collapse project",
    combo: { key: "ArrowLeft" },
    when: "tree",
  },
  {
    id: "tree.select",
    group: "tree",
    description: "Select row",
    combo: { key: "Enter" },
    when: "tree",
  },
  {
    id: "tree.select.space",
    group: "tree",
    description: "Select row",
    combo: { key: " " },
    when: "tree",
  },
  {
    id: "tree.newWorktree",
    group: "tree",
    description: "New worktree for selected project",
    combo: { key: "n", primary: true },
    when: "tree",
  },
  // ---- git (handlers land in slice 4) -----------------------------------
  {
    id: "git.up",
    group: "git",
    description: "Move up",
    combo: { key: "ArrowUp" },
    when: "git",
  },
  {
    id: "git.down",
    group: "git",
    description: "Move down",
    combo: { key: "ArrowDown" },
    when: "git",
  },
  {
    id: "git.stage",
    group: "git",
    description: "Stage/unstage focused file",
    combo: { key: " " },
    when: "git",
  },
  {
    id: "git.openDiff",
    group: "git",
    description: "Open diff for focused file",
    combo: { key: "Enter" },
    when: "git",
  },
  {
    id: "git.discard",
    group: "git",
    description: "Discard focused file",
    combo: { key: "Backspace" },
    when: "git",
  },
  {
    id: "git.refresh",
    group: "git",
    description: "Refresh status",
    combo: { key: "r" },
    when: "git",
  },
  // ←/→ switch the right-rail view (CHANGES ⇄ HISTORY) while the git pane is
  // focused. The git pane focus is SHARED by both views; the toggle decides what
  // is rendered. Like the other bare git keys these stay UNWIRED in the layout
  // dispatcher and are handled component-locally in RightRail (DOM focus is in
  // the pane), so the dispatcher lets them fall through. They exist here as the
  // documentation / Settings source and so matchBinding pane-gates them to git.
  {
    id: "git.viewPrev",
    group: "git",
    description: "Switch rail view (Changes ⇄ History)",
    combo: { key: "ArrowLeft" },
    when: "git",
  },
  {
    id: "git.viewNext",
    group: "git",
    description: "Switch rail view (Changes ⇄ History)",
    combo: { key: "ArrowRight" },
    when: "git",
  },
  {
    id: "git.commit",
    group: "git",
    description: "Open commit dialog",
    combo: { key: "Enter", primary: true },
    when: "git",
  },
];

// The set of bindings whose combos use a modifier. These are eligible from any
// focus (including the terminal's hidden textarea) — only bare-key bindings need
// the editable-target / pane gate. Used by both the matcher and terminalKeys.ts
// (which derives its "app" pass-through list from these — see below).
function hasModifier(combo: ComboSpec): boolean {
  return Boolean(combo.primary || combo.ctrl || combo.shift || combo.alt);
}

// The currently focused pane, default "terminal" (the app boots into the
// terminal). Updated by the focus-pane commands and by `focusin` handlers on the
// pane roots (slices 3/4). Lives here so the matcher and the components that set
// it share one definition.
export const focusedPane = writable<FocusedPane>("terminal");

// ---- terminal focus registry ------------------------------------------------
// The focus.terminal command must move DOM focus into the live xterm, but xterm
// instances are owned by per-session Terminal.svelte components (Center keeps
// one mounted per session and toggles visibility). Rather than reach into xterm
// internals from the layout, each Terminal registers a focus thunk keyed by its
// session id; the dispatcher focuses whichever session is active. Last writer
// per id wins; unregister on destroy. Kept here so keymap is the one place that
// knows how panes receive focus.
const terminalFocusers = new Map<string, () => void>();

// Register (or replace) the focus thunk for a session's terminal. Returns an
// unregister fn for onDestroy.
export function registerTerminalFocus(sessionId: string, focus: () => void): () => void {
  terminalFocusers.set(sessionId, focus);
  return () => {
    if (terminalFocusers.get(sessionId) === focus) terminalFocusers.delete(sessionId);
  };
}

// Focus the active session's terminal, if one is registered. The dispatcher
// passes the active session id (read from daemon state in the layout, so keymap
// stays free of a daemon import). No-op when nothing is registered (no live
// session / not yet mounted).
export function focusTerminal(activeSessionId: string | null): void {
  if (!activeSessionId) return;
  terminalFocusers.get(activeSessionId)?.();
}

// The minimal event shape the matcher reads, so tests can pass plain objects
// (mirrors terminalKeys.ts). A real KeyboardEvent satisfies this structurally.
export interface KeyEventLike {
  key: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  altKey: boolean;
}

// Case-insensitive compare for single-letter keys; exact for everything else
// (named keys, digits, punctuation). Browsers report letter `key` lowercased
// without Shift and uppercased with it, so "e" must match both "e" and "E".
function keyMatches(spec: string, actual: string): boolean {
  if (spec.length === 1 && /[a-z]/i.test(spec)) {
    return spec.toLowerCase() === actual.toLowerCase();
  }
  return spec === actual;
}

// Does this key event satisfy a combo's key + EXACT modifier requirements on the
// given platform? The single matching primitive, shared by matchBinding (pane
// gating is layered on top there) and terminalKeys.ts's app pass-through check.
//
// Modifiers are resolved to the four PHYSICAL flags the event carries, then
// compared exactly so every modifier is required-or-forbidden:
//  - `primary`  → metaKey on macOS, ctrlKey elsewhere
//  - `ctrl`     → literal ctrlKey on BOTH platforms (Ctrl+`/Ctrl+Tab)
//  - `shift`/`alt` → literal
// On non-mac, primary AND ctrl both map to ctrlKey, so `requiredCtrl` is the OR
// of the two — that's what makes Ctrl+` (primary off, ctrl on) match there even
// though Ctrl is also the primary modifier. No binding sets both flags on macOS,
// so the macOS branch keeps them independent (Cmd vs Ctrl).
export function comboMatchesEvent(
  combo: ComboSpec,
  e: KeyEventLike,
  platform: DesktopPlatform,
): boolean {
  if (!keyMatches(combo.key, e.key)) return false;
  const mac = platform === "macos";
  const requiredMeta = mac ? Boolean(combo.primary) : false;
  const requiredCtrl = mac
    ? Boolean(combo.ctrl)
    : Boolean(combo.primary) || Boolean(combo.ctrl);
  return (
    e.metaKey === requiredMeta &&
    e.ctrlKey === requiredCtrl &&
    e.shiftKey === Boolean(combo.shift) &&
    e.altKey === Boolean(combo.alt)
  );
}

// Pure resolver: given a key event, the platform, and the focused pane, return
// the matched binding or null. Discriminates modifiers EXACTLY — every modifier
// is required-or-forbidden, so Cmd+Shift+E never matches a Cmd+E binding and
// Cmd+B never matches Cmd+Alt+B. Bare-key bindings additionally require their
// `when` pane to be focused. The editable-target gate lives in the dispatcher
// (it needs the DOM target); this stays DOM-free and testable.
export function matchBinding(
  e: KeyEventLike,
  platform: DesktopPlatform = currentDesktopPlatform(),
  pane: FocusedPane = "terminal",
): Binding | null {
  for (const b of bindings) {
    if (!comboMatchesEvent(b.combo, e, platform)) continue;
    // Bare-key (no-modifier) bindings are pane-gated.
    if (b.when && !hasModifier(b.combo) && pane !== b.when) continue;
    return b;
  }
  return null;
}

// ---- app-combo list for the xterm classifier --------------------------------
// terminalKeys.ts must classify every app-level COMBO (those with a modifier)
// as "app" so attachCustomKeyEventHandler returns false and xterm never
// processes them — the window-capture dispatcher has already handled them.
// Deriving the list FROM `bindings` here (rather than re-listing combos in
// terminalKeys.ts) means the two can't drift. terminalKeys.ts imports this; the
// import is acyclic (keymap.ts does not import terminalKeys.ts).
//
// Only modifier combos are included: bare-key bindings (arrows/Space/Enter/R)
// are pane-local and must reach the terminal (or be ignored) normally — they
// are NOT app pass-through, or they'd be swallowed while the terminal is typed
// into. Esc is likewise omitted (the terminal needs Esc for vim/TUIs).
//
// Pane-gated modifier combos (those with a `when`) are ALSO excluded: git.commit
// (Cmd+Enter, when="git") and tree.newWorktree (Cmd+N, when="tree") only act
// inside their pane, and the layout dispatcher gates them there. Treating them
// as app pass-through would make xterm swallow Cmd+Enter / Cmd+N while typing in
// the terminal even though the dispatcher declines to act on them. So the app
// list is exactly the modifier combos WITHOUT a `when` (the global/tabs combos
// in PLAN.md's app list: Cmd+1–9, Cmd+W, Cmd+T, Cmd+B, Cmd+Alt+B,
// Cmd+Shift+E/G/[/], Ctrl+`, Ctrl+Tab/Ctrl+Shift+Tab, plus Cmd+K and Cmd+,).
export const appCombos: readonly ComboSpec[] = bindings
  .filter((b) => !b.when)
  .map((b) => b.combo)
  .filter(hasModifier);

// Display metadata for the Settings reference panel (slice 5): render a combo as
// an ordered list of keycap labels, platform-aware. Modifier order follows the
// platform convention (Ctrl/⌘ first, then Alt/Option, then Shift, then the
// key). `appCombos`/`bindings` stay the source of truth so the reference can't
// drift from behavior.
export function comboKeys(combo: ComboSpec, platform: DesktopPlatform): string[] {
  const keys: string[] = [];
  if (combo.ctrl && !combo.primary) keys.push("Ctrl");
  if (combo.primary) keys.push(shortcutModifierLabel(platform));
  if (combo.alt) keys.push(platform === "macos" ? "⌥" : "Alt");
  if (combo.shift) keys.push(platform === "macos" ? "⇧" : "Shift");
  keys.push(comboKeyLabel(combo.key));
  return keys;
}

// Display label for a combo's primary key (the named/printable cap).
function comboKeyLabel(key: string): string {
  switch (key) {
    case " ":
      return "␣";
    case "Escape":
      return "Esc";
    case "ArrowUp":
      return "↑";
    case "ArrowDown":
      return "↓";
    case "ArrowLeft":
      return "←";
    case "ArrowRight":
      return "→";
    case "Enter":
      return "↵";
    case "Backspace":
      return "⌫";
    default:
      return key.length === 1 ? key.toUpperCase() : key;
  }
}
