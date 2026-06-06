import { currentDesktopPlatform, isShortcutModifier, type DesktopPlatform } from "./desktopPlatform";
import { appCombos, comboMatchesEvent, type ComboSpec } from "./keymap";

// Pure classifier for the terminal's platform-specific shortcuts, split out of
// Terminal.svelte so the keyboard-routing decision is unit-testable WITHOUT a
// live xterm/DOM. The real `attachCustomKeyEventHandler` calls this to decide
// intent, then applies the side effects (copy needs a selection check, search
// opens the overlay, newline writes a \n, suppress blocks duplicate native
// Shift+Enter phases). The load-bearing property: paste shortcuts are
// classified as "pass" — paste falls through to xterm's native textarea paste
// (which already honors bracketed-paste), so there is exactly ONE keyboard
// paste route and it cannot double-fire.

export type TerminalKeyAction = "copy" | "search" | "newline" | "suppress" | "pass" | "app";

export type TerminalShortcutPlatform = DesktopPlatform;

// Read-only reference for the Settings Keybindings panel (slice 5). These are
// the terminal-INTERNAL keys handled by classifyTerminalKey above — they are
// NOT keymap bindings (they never appear in keymap.ts `bindings`/`appCombos`),
// so the settings panel sources them from here instead of hardcoding strings,
// keeping the reference next to the behavior it describes. `combo` is built with
// the same ComboSpec shape keymap.comboKeys consumes, so the panel renders these
// chips through the exact same code path as every other binding.
//
// Copy/find use the platform primary modifier (Cmd on macOS, Ctrl elsewhere);
// off-mac they additionally require Shift (Ctrl+Shift+C/F) to avoid clobbering
// Ctrl+C/Ctrl+F which must reach the child process. `description`/combo mirror
// the classifyTerminalKey branches above.
export interface TerminalKeyRef {
  description: string;
  combo: ComboSpec;
}

export function terminalKeyReference(platform: DesktopPlatform): TerminalKeyRef[] {
  const mac = platform === "macos";
  return [
    {
      description: "Copy selection",
      combo: { key: "c", primary: true, shift: !mac },
    },
    {
      description: "Find in terminal",
      combo: { key: "f", primary: true, shift: !mac },
    },
    {
      description: "Insert newline (without submitting)",
      combo: { key: "Enter", shift: true },
    },
  ];
}

// Does this event match an app-level keymap combo (e.g. Cmd+1, Cmd+W, Ctrl+`)?
// Those are dispatched by the window-capture listener in +layout.svelte, so
// xterm must NOT process them — the classifier returns "app" and
// attachCustomKeyEventHandler returns false. The combo list is derived from
// keymap.ts's `appCombos` so the terminal pass-through set can never drift from
// the keymap. Mirrors the (exact-modifier) matching in keymap.matchBinding, but
// pane-independent: app combos pass through regardless of focus.
function matchesAppCombo(
  e: { metaKey: boolean; ctrlKey: boolean; shiftKey: boolean; altKey?: boolean; key: string },
  platform: DesktopPlatform,
): boolean {
  // Reuse the keymap's matching primitive so the pass-through set and the
  // dispatcher agree on exactly which combos are app-level (no drift). Read the
  // fields EXPLICITLY — spreading a real KeyboardEvent copies nothing (its
  // properties live as prototype getters, not own-enumerable props), which made
  // `key` undefined and threw inside xterm's keydown handler for every key,
  // breaking keydown-only keys (Backspace, Ctrl+C) in the live terminal while
  // unit tests (plain objects, where spread works) stayed green. altKey is
  // optional on the classifier's event shape; default absent to false.
  const ev = {
    key: e.key,
    metaKey: e.metaKey,
    ctrlKey: e.ctrlKey,
    shiftKey: e.shiftKey,
    altKey: e.altKey ?? false,
  };
  return appCombos.some((c) => comboMatchesEvent(c, ev, platform));
}

// Classify a keyboard event into the action the handler should take. Only the
// fields the decision needs are required, so tests can pass plain objects.
export function classifyTerminalKey(
  e: {
    type: string;
    metaKey: boolean;
    ctrlKey: boolean;
    shiftKey: boolean;
    // altKey is optional so existing callers/tests need not set it; only the
    // app-combo check (Cmd+Alt+B) reads it, defaulting absent to false.
    altKey?: boolean;
    key: string;
  },
  platform: TerminalShortcutPlatform = currentDesktopPlatform(),
): TerminalKeyAction {
  // Shift+Enter (keydown) → newline (\n) so apps can tell it from Enter (\r).
  // Later phases for the same physical keypress must still be suppressed:
  // xterm/browser key event behavior differs by platform, and passing them
  // through can also emit the native Enter path (\r), turning "insert newline"
  // into "insert newline, then submit".
  if (e.shiftKey && e.key === "Enter") {
    return e.type === "keydown" ? "newline" : "suppress";
  }
  // Shortcuts are intercepted on keydown only. macOS uses Cmd/meta; Windows,
  // Linux, and other desktop platforms use Ctrl. Plain Ctrl+C/Ctrl+F must pass
  // through off macOS so they can reach the child process; Ctrl+Shift+C and
  // Ctrl+Shift+F are the non-mac terminal copy/search shortcuts.
  if (e.type !== "keydown") return "pass";
  // App-level combos (Cmd+1–9, Cmd+W/T, Cmd+B, Cmd+Alt+B, Cmd+Shift+E/G/[/],
  // Ctrl+`, Ctrl+Tab, Cmd+K, Cmd+,) are owned by the window-capture dispatcher;
  // xterm must never process them, so consume them here ("app" → handler returns
  // false). Checked before the terminal's own copy/search because those are NOT
  // keymap bindings (they stay terminal-internal) and so are never in appCombos.
  // Ctrl+` and Ctrl+Tab carry no platform-primary modifier, so this must run
  // before the shortcutHeld bail below.
  if (matchesAppCombo(e, platform)) return "app";
  const shortcutHeld = isShortcutModifier(e, platform);
  if (!shortcutHeld) return "pass";
  if (e.key === "c" || e.key === "C") {
    return platform === "macos" || e.shiftKey ? "copy" : "pass";
  }
  if (e.key === "f" || e.key === "F") {
    return platform === "macos" || e.shiftKey ? "search" : "pass";
  }
  // Paste shortcuts are intentionally NOT special: they pass through to native
  // xterm paste.
  return "pass";
}
