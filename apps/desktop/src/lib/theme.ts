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

import { derived, type Readable, type Writable } from "svelte/store";
import { invoke } from "@tauri-apps/api/core";
import { persisted } from "./settings";

export type Theme = "light" | "dark";

const STORAGE_KEY = "hitch-theme";

/**
 * First-run default when nothing is saved: follow the OS appearance so a
 * dark-OS user starts in dusk. Without this, the store would default to "light"
 * and undo the dark prepaint that app.html (prefers-color-scheme) and the Rust
 * side (os_appearance) already resolved — a dark→light flash at mount. Guarded
 * for SSR/test envs where matchMedia is undefined. (persisted() writes this back
 * to localStorage on init, which records the OS appearance as the chosen value,
 * consistent with the Rust mirror.)
 */
function osDefaultTheme(): Theme {
  return typeof matchMedia !== "undefined" &&
    matchMedia("(prefers-color-scheme: dark)").matches
    ? "dark"
    : "light";
}

/**
 * Mirror the theme into a file the Rust side reads synchronously at the next
 * launch, so the native window background is painted from the persisted theme
 * before the webview loads (no light flash for dark-theme users). Fire-and-forget
 * and silently ignored outside the Tauri context (e.g. plain browser/test runs).
 */
function mirrorThemeToBackend(value: Theme): void {
  void invoke("set_window_theme", { theme: value }).catch(() => {});
}

/**
 * The active theme. Backed by localStorage via the shared `persisted` helper,
 * so it reads the saved value at creation (same "hitch-theme" key the app.html
 * prepaint script reads) and writes back on every change — normalizing anything
 * other than "dark" to light. With no saved value the initial is the OS
 * appearance (see osDefaultTheme), matching the prepaint/native-frame default.
 */
export const theme = persisted(STORAGE_KEY, osDefaultTheme(), (value) =>
  value === "dark" ? "dark" : "light",
) as Writable<Theme>;

/**
 * Whether the active theme is dusk (dark). Derived from the store value so
 * consumers never re-read the DOM `data-theme` attribute to learn the mode:
 * the store already holds the resolved "light"/"dark", and applyTheme() (wired
 * by initTheme()) keeps the attribute in sync with it. Reactive contexts read
 * `$isDark`; non-reactive code paths read `get(isDark)`.
 */
export const isDark: Readable<boolean> = derived(theme, ($t) => $t === "dark");

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
 * Initialize the theme on mount: the store already holds the persisted value
 * (read at creation, default light) and writes back to localStorage itself, so
 * this just attaches the document/backend side-effects. The subscribe fires
 * synchronously with the current value, applying it to <html> right away, then
 * on every future store change. Idempotent enough to call once from the layout.
 */
export function initTheme(): void {
  theme.subscribe((value) => {
    applyTheme(value);
    // Seed/refresh the Rust-readable mirror so the native window picks up the
    // current theme on the next launch — including for users who already had a
    // theme saved before the mirror existed (this fires on init too).
    mirrorThemeToBackend(value);
  });
}

/** Flip between paper (light) and dusk (dark). */
export function toggleTheme(): void {
  theme.update((value) => (value === "dark" ? "light" : "dark"));
}
