// Drag-and-drop of OS files onto a terminal: dropping files inserts their
// (shell-escaped) absolute paths at the active shell prompt — the same thing
// every native terminal (Terminal.app, iTerm2, WezTerm) does.
//
// Why a single global listener and not a DOM ondrop on each .term-host: in a
// Tauri webview the native window layer intercepts OS file drags BEFORE the
// webview sees them, so the browser drag events (ondrop/ondragover) never fire
// for real files. The only signal is Tauri's window-global `onDragDropEvent`,
// which fires ONCE per drag regardless of which pane it's over and carries no
// target element — just file paths and a physical cursor position. We turn
// that position back into a target by hit-testing the DOM (`elementFromPoint`)
// and walking up to the nearest `[data-session-id]` (set on each terminal's
// host). A drop that lands on a rail, the diff, or empty space resolves to no
// host and is ignored — "under-cursor" routing with a single listener.
import { getCurrentWebviewWindow } from "@tauri-apps/api/webviewWindow";
import type { UnlistenFn } from "@tauri-apps/api/event";
import { writable } from "svelte/store";
import { sendInput } from "./daemon";
import type { Id } from "./types";

// The terminal currently under a file drag (or null). Terminal.svelte subscribes
// and paints a drop-target ring so the user can see exactly where the paths will
// land before releasing.
export const dropTargetSession = writable<Id | null>(null);

// Windows uses backslash as the path separator, so POSIX backslash-escaping
// would corrupt paths there; quoting style is chosen per-platform. The webview
// UA string is the dependency-free way to tell — `@tauri-apps/plugin-os` would
// be the alternative but isn't installed.
function isWindows(): boolean {
  return /Windows/i.test(navigator.userAgent);
}

// POSIX: backslash-escape every character outside a conservative safe set so
// bash AND fish both treat the whole path as a single literal word — spaces,
// parens, `$`, quotes, globs, everything. This matches what dragging a file
// into Terminal.app produces and is safe against paths like `$(rm -rf ~)`.
function escapePosix(path: string): string {
  return path.replace(/[^A-Za-z0-9_./:@%+,=-]/g, (c) => `\\${c}`);
}

// Windows: a filename can't legally contain a double quote, so wrapping in
// double quotes is lossless. Only quote when the path holds a character that
// would otherwise break cmd/PowerShell tokenization; bare paths stay bare.
function quoteWindows(path: string): string {
  return /[\s&()[\]{}^=;!'+,`~%]/.test(path) ? `"${path}"` : path;
}

// Turn the dropped absolute paths into the text to inject at the prompt:
// each path escaped/quoted for the host shell, space-separated, with a trailing
// space so the next path (or typed argument) doesn't run into the last one.
export function formatDroppedPaths(
  paths: string[],
  windows = isWindows(),
): string {
  const escape = windows ? quoteWindows : escapePosix;
  return paths.map(escape).join(" ") + " ";
}

// Resolve the terminal session under a physical (device-pixel) cursor position,
// or null if the point isn't over a terminal. Tauri reports the drop point in
// physical pixels relative to the window's top-left; `elementFromPoint` wants
// CSS pixels relative to the viewport top-left. The webview fills the window
// (Overlay titlebar — no native chrome offset), so dividing by devicePixelRatio
// is the only conversion needed. (Known Tauri caveat: the position can be off
// while devtools is docked open; fine in production.)
function sessionAtPosition(position: { x: number; y: number }): Id | null {
  const dpr = window.devicePixelRatio || 1;
  const el = document.elementFromPoint(position.x / dpr, position.y / dpr);
  const host = el?.closest<HTMLElement>("[data-session-id]");
  return host?.dataset.sessionId ?? null;
}

// Register the single app-wide file-drop listener. Call once from the root
// layout's onMount and invoke the returned unlisten on teardown.
export async function initFileDrop(): Promise<UnlistenFn> {
  return getCurrentWebviewWindow().onDragDropEvent((event) => {
    const payload = event.payload;
    switch (payload.type) {
      case "enter":
      case "over":
        // Track the hovered terminal so the highlight follows the cursor across
        // panes during the drag.
        dropTargetSession.set(sessionAtPosition(payload.position));
        break;
      case "leave":
        dropTargetSession.set(null);
        break;
      case "drop": {
        dropTargetSession.set(null);
        const sessionId = sessionAtPosition(payload.position);
        // Dropped outside any terminal (rail/diff/empty), or an empty drag.
        if (!sessionId || payload.paths.length === 0) return;
        sendInput(sessionId, formatDroppedPaths(payload.paths));
        break;
      }
    }
  });
}
