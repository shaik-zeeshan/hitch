// Desktop-local user preferences, persisted in localStorage. The daemon owns
// no chrome preferences (like editor app), while daemon actions receive the
// relevant user-selected settings explicitly in their IPC requests.
import { writable, type Writable } from "svelte/store";

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

function persisted(
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
// best-effort localStorage handling. Writes clamp into range so a stored value
// can never drift out of bounds.
function persistedNumber(
  key: string,
  initial: number,
  min: number,
  max: number,
): Writable<number> {
  const clamp = (value: number) => Math.min(max, Math.max(min, Math.round(value)));
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
      localStorage.setItem(key, String(clamp(value)));
    } catch {}
  });
  return store;
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
