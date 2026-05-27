// Desktop-local user preferences, persisted in localStorage. The daemon owns
// no notion of these (they're client chrome, like the overlays store), so they
// live here rather than in the daemon contract. Today that's just the editor
// used by the worktree "Open in editor" action.
import { writable, type Writable } from "svelte/store";

const EDITOR_KEY = "hitch.editorApp";

// Application name handed to the OS "open with" (macOS: `open -a <app>`). An
// app name — not a `code`-style CLI shim — so it's PATH-independent and works
// for any installed editor (Cursor, Zed, Sublime…) by name.
export const DEFAULT_EDITOR = "Visual Studio Code";

function persisted(key: string, initial: string): Writable<string> {
  let start = initial;
  try {
    start = localStorage.getItem(key) ?? initial;
  } catch {
    // localStorage can be unavailable (private mode / denied); fall back to the
    // default and keep an in-memory store for the session.
  }
  const store = writable(start);
  store.subscribe((value) => {
    try {
      localStorage.setItem(key, value);
    } catch {
      // See above — writes are best-effort.
    }
  });
  return store;
}

export const editorApp = persisted(EDITOR_KEY, DEFAULT_EDITOR);
