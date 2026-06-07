// Desktop-local user preferences, persisted in localStorage. The daemon owns
// no chrome preferences (like editor app), while daemon actions receive the
// relevant user-selected settings explicitly in their IPC requests.
import { get, writable, type Writable } from "svelte/store";

const EDITOR_KEY = "hitch.editorApp";
const DRAFT_PROVIDER_KEY = "hitch.draftProvider";
const DRAFT_MODEL_KEY = "hitch.draftModel";
const DRAFT_CLAUDE_PATH_KEY = "hitch.draftClaudePath";
const DRAFT_CODEX_PATH_KEY = "hitch.draftCodexPath";
const AUTO_COMMIT_PUSH_KEY = "hitch.autoCommitPush";
const DIFF_STYLE_KEY = "hitch.diffStyle";
const DIFF_WRAP_KEY = "hitch.diffWrap";
const DIFF_IGNORE_WHITESPACE_KEY = "hitch.diffIgnoreWhitespace";
const DIFF_CONTEXT_LINES_KEY = "hitch.diffContextLines";
const TERM_FONT_FAMILY_KEY = "hitch.termFontFamily";
const NOTIFICATION_MODE_KEY = "hitch.notificationMode";
const NOTIFICATION_MIN_TURN_SECONDS_KEY = "hitch.notificationMinTurnSeconds";

// Editor preference passed to the desktop backend. Empty string is the
// default and means "System default": the backend resolves $VISUAL/$EDITOR at
// launch time (and errors when neither is set). Named editors or an explicit
// executable path remain explicit choices.
export const SYSTEM_DEFAULT_EDITOR = "";

export type DraftProvider = "stub" | "claude" | "codex";
export const DEFAULT_DRAFT_PROVIDER: DraftProvider = "stub";
export const DEFAULT_DRAFT_MODEL = "";

// Offline FALLBACK only. The daemon's `list-draft-models` IPC is the
// authoritative model list per provider (it mirrors the CLI's own aliases);
// the settings page fetches it live and falls back to these minimal lists only
// on error/timeout, so they don't need to stay exhaustive or in sync.
export const DRAFT_MODEL_OPTIONS: Record<DraftProvider, string[]> = {
  stub: ["stub"],
  claude: ["default", "sonnet", "opus", "haiku"],
  codex: ["gpt-5-codex", "gpt-5", "gpt-5-mini"],
};

// localStorage-backed writable (best-effort reads/writes). Exported and reused
// by terminal-themes.ts so the persistence behavior (SSR/private-mode guard,
// best-effort writes) lives in one place. `normalize` lets callers sanitize a
// stored value on read; it defaults to identity.
export function persisted(
  key: string,
  initial: string,
  normalize: (value: string) => string = (value) => value,
): Writable<string> {
  let start = initial;
  try {
    start = normalize(localStorage.getItem(key) ?? initial);
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

// `null` means the user has never explicitly picked a provider, so draft
// requests omit the provider override and the daemon falls back to its own
// configured default (`--draft-provider` / `HITCH_DRAFT_PROVIDER`). A concrete
// value is only ever stored when the user saves a choice — we deliberately do
// NOT write the default back on init, which would mask "unset" as "chose stub".
function persistedDraftProvider(): Writable<DraftProvider | null> {
  let start: DraftProvider | null = null;
  try {
    const stored = localStorage.getItem(DRAFT_PROVIDER_KEY);
    start = stored && isDraftProvider(stored) ? stored : null;
  } catch {
    // localStorage unavailable; keep an in-memory store.
  }
  const store = writable<DraftProvider | null>(start);
  store.subscribe((value) => {
    try {
      if (value) localStorage.setItem(DRAFT_PROVIDER_KEY, value);
    } catch {
      // Best-effort persistence.
    }
  });
  return store;
}

function isDraftProvider(value: string): value is DraftProvider {
  return value === "stub" || value === "claude" || value === "codex";
}

function persistedBool(key: string, initial: boolean): Writable<boolean> {
  let start = initial;
  try {
    const stored = localStorage.getItem(key);
    if (stored !== null) start = stored === "true";
  } catch {}
  const store = writable(start);
  store.subscribe((value) => {
    try {
      localStorage.setItem(key, String(value));
    } catch {}
  });
  return store;
}

// Numeric preference, persisted as a string. Reads that don't parse (or fall
// outside [min, max]) fall back to the default, mirroring `persisted`'s
// best-effort localStorage handling. The store clamps on EVERY write (set and
// update), not just on persistence, so the in-memory value consumers read via
// `get(...)` can never drift out of bounds — a settings input typing/pasting an
// out-of-range value lands clamped both in memory and in localStorage.
function persistedNumber(
  key: string,
  initial: number,
  min: number,
  max: number,
): Writable<number> {
  const clamp = (value: number) =>
    Number.isFinite(value) ? Math.min(max, Math.max(min, Math.round(value))) : initial;
  let start = initial;
  try {
    const stored = localStorage.getItem(key);
    if (stored !== null) {
      const parsed = Number(stored);
      if (Number.isFinite(parsed)) start = clamp(parsed);
    }
  } catch {
    // localStorage unavailable; keep an in-memory store with the default.
  }
  const store = writable(start);
  store.subscribe((value) => {
    try {
      // Value is already clamped by `set` below, so persist it as-is.
      localStorage.setItem(key, String(value));
    } catch {}
  });
  // Wrap `set` to clamp before storing. A bound `<input>` may write the clamped
  // value straight back (e.g. typing 9999 clamps to 600, which Svelte echoes
  // into the input); skip that no-op write so we don't churn subscribers.
  const set = (value: number) => {
    const clamped = clamp(value);
    if (clamped !== get(store)) store.set(clamped);
  };
  return {
    subscribe: store.subscribe,
    set,
    update: (fn) => set(fn(get(store))),
  };
}

export type DiffStyle = "unified" | "split";
export const DEFAULT_DIFF_STYLE: DiffStyle = "unified";
export const DEFAULT_DIFF_CONTEXT_LINES = 3;
export const DIFF_CONTEXT_LINES_MIN = 0;
export const DIFF_CONTEXT_LINES_MAX = 999;

function isDiffStyle(value: string): value is DiffStyle {
  return value === "unified" || value === "split";
}

export const editorApp = persisted(EDITOR_KEY, SYSTEM_DEFAULT_EDITOR);
export const draftProvider = persistedDraftProvider();
export const draftModel = persisted(DRAFT_MODEL_KEY, DEFAULT_DRAFT_MODEL);
export const draftClaudePath = persisted(DRAFT_CLAUDE_PATH_KEY, "");
export const draftCodexPath = persisted(DRAFT_CODEX_PATH_KEY, "");
export const autoCommitPush = persistedBool(AUTO_COMMIT_PUSH_KEY, false);

// Diff view preferences. The render-side pair (`diffStyle`, `diffWrap`) only
// affects how an already-fetched diff is laid out by @pierre/diffs. The
// re-diff pair (`diffIgnoreWhitespace`, `diffContextLines`) changes the
// daemon's `git diff` invocation, so toggling them re-fetches the diff text.
export const diffStyle = persisted(DIFF_STYLE_KEY, DEFAULT_DIFF_STYLE, (value) =>
  isDiffStyle(value) ? value : DEFAULT_DIFF_STYLE,
) as Writable<DiffStyle>;
export const diffWrap = persistedBool(DIFF_WRAP_KEY, false);
export const diffIgnoreWhitespace = persistedBool(DIFF_IGNORE_WHITESPACE_KEY, false);
export const diffContextLines = persistedNumber(
  DIFF_CONTEXT_LINES_KEY,
  DEFAULT_DIFF_CONTEXT_LINES,
  DIFF_CONTEXT_LINES_MIN,
  DIFF_CONTEXT_LINES_MAX,
);

// Terminal font family. Empty string = the built-in stack below; a concrete
// value is an installed family the user picked in Settings (typically a Nerd
// Font, so dev icons render — see terminalFontStack).
export const DEFAULT_TERM_FONT_FAMILY = "";
export const terminalFontFamily = persisted(TERM_FONT_FAMILY_KEY, DEFAULT_TERM_FONT_FAMILY);

// Which right-rail view is active: CHANGES (working tree) or HISTORY (commit
// log). A session-only UI selection — it survives worktree switches within a
// run but is deliberately NOT persisted, so every app launch starts at Changes
// (the main view). History is purely for browsing the commit log; restoring it
// on relaunch would strand a worktree away from its working-tree changes. The
// log itself is per-worktree and refetches on selection.
export type RailView = "changes" | "history";
export const DEFAULT_RAIL_VIEW: RailView = "changes";

export const railView = writable<RailView>(DEFAULT_RAIL_VIEW);

// Base terminal stack: the shell's --mono stack (JetBrains Mono, app.css),
// with the symbols-only Nerd Fonts appended as icon fallbacks. The bundled
// JetBrains Mono is NOT Nerd-patched, and most Nerd Font icons live in
// supplementary-plane PUA codepoints (U+F0001–U+F1AF0) that exist ONLY in
// Nerd Fonts — WebKit renders tofu for them otherwise. Text glyphs still come
// from the first font; only the icon codepoints fall through to the symbols
// font when the user has it installed.
const TERM_FONT_BASE =
  '"JetBrains Mono", ui-monospace, "SF Mono", Menlo, "Symbols Nerd Font Mono", "Symbols Nerd Font", monospace';

// The font-family string terminal panes actually render with (xterm options +
// the daemon's cell-measurement probe — both MUST use the same stack so the
// estimated grid matches the real one). A picked family goes FIRST so both its
// text and its icon glyphs win; the base stack stays behind it as fallback.
export function terminalFontStack(family: string): string {
  // Strip quotes rather than escaping them: a family name with embedded
  // quotes is never legitimate, and a broken value here would silently
  // invalidate the whole font-family declaration.
  const custom = family.trim().replace(/["']/g, "");
  return custom ? `"${custom}", ${TERM_FONT_BASE}` : TERM_FONT_BASE;
}

// Native OS notification suppression policy (notifications.ts fires on live
// agent-state transitions). Three tiers, default the middle one so a focused
// user watching the very session that changed isn't pinged, but anything else
// (app backgrounded, or a *different* session acting) still surfaces:
//  - "off": never notify.
//  - "app-in-background": notify only when the app window is unfocused.
//  - "background-or-other-session": notify unless the window is focused AND the
//    session that changed is the one currently visible in the UI.
export type NotificationMode = "off" | "app-in-background" | "background-or-other-session";
export const DEFAULT_NOTIFICATION_MODE: NotificationMode = "background-or-other-session";

function isNotificationMode(value: string): value is NotificationMode {
  return (
    value === "off" || value === "app-in-background" || value === "background-or-other-session"
  );
}

export const notificationMode = persisted(
  NOTIFICATION_MODE_KEY,
  DEFAULT_NOTIFICATION_MODE,
  (value) => (isNotificationMode(value) ? value : DEFAULT_NOTIFICATION_MODE),
) as Writable<NotificationMode>;

// Minimum turn duration (seconds) before a `running` → `waiting` "finished"
// notification fires — short turns (a one-line answer) don't warrant an OS
// ping. 0 means ungated (every turn end notifies). Clamped to a sane ceiling so
// a stored value can't push the gate so high that "finished" never fires.
export const DEFAULT_NOTIFICATION_MIN_TURN_SECONDS = 30;
export const NOTIFICATION_MIN_TURN_SECONDS_MIN = 0;
export const NOTIFICATION_MIN_TURN_SECONDS_MAX = 600;
export const notificationMinTurnSeconds = persistedNumber(
  NOTIFICATION_MIN_TURN_SECONDS_KEY,
  DEFAULT_NOTIFICATION_MIN_TURN_SECONDS,
  NOTIFICATION_MIN_TURN_SECONDS_MIN,
  NOTIFICATION_MIN_TURN_SECONDS_MAX,
);
