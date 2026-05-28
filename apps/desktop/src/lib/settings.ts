// Desktop-local user preferences, persisted in localStorage. The daemon owns
// no chrome preferences (like editor app), while daemon actions receive the
// relevant user-selected settings explicitly in their IPC requests.
import { writable, type Writable } from "svelte/store";

const EDITOR_KEY = "hitch.editorApp";
const DRAFT_PROVIDER_KEY = "hitch.draftProvider";
const DRAFT_MODEL_KEY = "hitch.draftModel";

// Application name handed to the OS "open with" (macOS: `open -a <app>`). An
// app name — not a `code`-style CLI shim — so it's PATH-independent and works
// for any installed editor (Cursor, Zed, Sublime…) by name.
export const DEFAULT_EDITOR = "Visual Studio Code";

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

export const editorApp = persisted(EDITOR_KEY, DEFAULT_EDITOR);
export const draftProvider = persistedDraftProvider();
export const draftModel = persisted(DRAFT_MODEL_KEY, DEFAULT_DRAFT_MODEL);
