// Pure classifier for the terminal's macOS Cmd-key shortcuts, split out of
// Terminal.svelte so the keyboard-routing decision is unit-testable WITHOUT a
// live xterm/DOM. The real `attachCustomKeyEventHandler` calls this to decide
// intent, then applies the side effects (copy needs a selection check, search
// opens the overlay, newline writes a \n). The load-bearing property: Cmd+V is
// classified as "pass" — paste is no longer a special action, it falls through
// to xterm's native textarea paste (which already honors bracketed-paste), so
// there is exactly ONE keyboard paste route and it cannot double-fire.

export type TerminalKeyAction = "copy" | "search" | "newline" | "pass";

// Classify a keyboard event into the action the handler should take. Only the
// fields the decision needs are required, so tests can pass plain objects.
export function classifyTerminalKey(e: {
  type: string;
  metaKey: boolean;
  shiftKey: boolean;
  key: string;
}): TerminalKeyAction {
  // Shift+Enter (keydown) → newline (\n) so apps can tell it from Enter (\r).
  if (e.shiftKey && e.key === "Enter") {
    return e.type === "keydown" ? "newline" : "pass";
  }
  // Everything below is a Cmd shortcut on keydown only; never intercept Ctrl
  // (so Ctrl+C stays SIGINT) and never the keyup/keypress phases.
  if (e.type !== "keydown" || !e.metaKey) return "pass";
  if (e.key === "c") return "copy"; // caller still gates on hasSelection()
  if (e.key === "f") return "search";
  // Cmd+V is intentionally NOT special: it passes through to native xterm paste.
  return "pass";
}
