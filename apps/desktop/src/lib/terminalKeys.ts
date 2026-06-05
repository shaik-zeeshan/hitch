import { currentDesktopPlatform, isShortcutModifier, type DesktopPlatform } from "./desktopPlatform";

// Pure classifier for the terminal's platform-specific shortcuts, split out of
// Terminal.svelte so the keyboard-routing decision is unit-testable WITHOUT a
// live xterm/DOM. The real `attachCustomKeyEventHandler` calls this to decide
// intent, then applies the side effects (copy needs a selection check, search
// opens the overlay, newline writes a \n, suppress blocks duplicate native
// Shift+Enter phases). The load-bearing property: paste shortcuts are
// classified as "pass" — paste falls through to xterm's native textarea paste
// (which already honors bracketed-paste), so there is exactly ONE keyboard
// paste route and it cannot double-fire.

export type TerminalKeyAction = "copy" | "search" | "newline" | "suppress" | "pass";

export type TerminalShortcutPlatform = DesktopPlatform;

// Classify a keyboard event into the action the handler should take. Only the
// fields the decision needs are required, so tests can pass plain objects.
export function classifyTerminalKey(
  e: {
    type: string;
    metaKey: boolean;
    ctrlKey: boolean;
    shiftKey: boolean;
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
