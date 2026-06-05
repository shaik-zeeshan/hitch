// Theme infrastructure for the Paper Terminal shell.
//
// Two themes: the warm "paper" light theme (default) and a "dusk" dark theme.
// The light theme is the absence of any attribute; the dark theme is selected
// by `data-theme="dark"` on <html>, matching the token tables in
// doc-design/colors.md (`:root` = light, `html[data-theme="dark"]` = dark).
//
// The choice persists to localStorage so it survives reloads. Call initTheme()
// once on mount (see +layout.svelte) to apply the persisted/default value to the
// document; toggleTheme() flips and persists.

import { writable } from "svelte/store";

export type Theme = "light" | "dark";

const STORAGE_KEY = "hitch-theme";

/** The active theme. Default is light ("paper"). */
export const theme = writable<Theme>("light");

function readPersisted(): Theme {
  if (typeof localStorage === "undefined") return "light";
  const saved = localStorage.getItem(STORAGE_KEY);
  return saved === "dark" ? "dark" : "light";
}

/** Apply a theme to <html> (data-theme="dark" for dusk, removed for paper). */
function applyTheme(value: Theme): void {
  if (typeof document === "undefined") return;
  const root = document.documentElement;
  if (value === "dark") {
    root.setAttribute("data-theme", "dark");
  } else {
    root.removeAttribute("data-theme");
  }
}

/**
 * Initialize the theme on mount: read the persisted value (default light),
 * apply it to the document, and keep <html> + localStorage in sync with every
 * future store change. Idempotent enough to call once from the root layout.
 */
export function initTheme(): void {
  const initial = readPersisted();
  theme.set(initial);
  theme.subscribe((value) => {
    applyTheme(value);
    if (typeof localStorage !== "undefined") {
      localStorage.setItem(STORAGE_KEY, value);
    }
  });
}

/** Flip between paper (light) and dusk (dark). */
export function toggleTheme(): void {
  theme.update((value) => (value === "dark" ? "light" : "dark"));
}
