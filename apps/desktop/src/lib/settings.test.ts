import { beforeEach, describe, expect, it, vi } from "vitest";
import { get } from "svelte/store";

// Minimal localStorage stand-in: the vitest config runs in a node environment
// (vitest.config.ts) without a DOM, so settings.ts's localStorage access needs
// a stub — mirrors desktopPlatform.test.ts.
class LocalStorageStub {
  readonly values = new Map<string, string>();

  getItem(key: string): string | null {
    return this.values.get(key) ?? null;
  }

  setItem(key: string, value: string): void {
    this.values.set(key, value);
  }
}

const MIN_TURN_KEY = "hitch.notificationMinTurnSeconds";

beforeEach(() => {
  vi.unstubAllGlobals();
  vi.resetModules();
});

async function loadSettings(stored?: Record<string, string>) {
  const storage = new LocalStorageStub();
  for (const [key, value] of Object.entries(stored ?? {})) {
    storage.values.set(key, value);
  }
  vi.stubGlobal("localStorage", storage);
  const settings = await import("./settings");
  return { storage, settings };
}

describe("persistedNumber clamp-on-write", () => {
  it("clamps an out-of-range typed value in the in-memory store", async () => {
    const { storage, settings } = await loadSettings();

    // Simulates a settings input writing a typed/pasted value past max=600.
    settings.notificationMinTurnSeconds.set(9999);

    // Consumers read the in-memory value via get(...) — it MUST be clamped, not
    // the raw 9999 that previously suppressed every "finished" notification.
    expect(get(settings.notificationMinTurnSeconds)).toBe(
      settings.NOTIFICATION_MIN_TURN_SECONDS_MAX,
    );
    expect(storage.values.get(MIN_TURN_KEY)).toBe(
      String(settings.NOTIFICATION_MIN_TURN_SECONDS_MAX),
    );
  });

  it("clamps below the minimum and rounds to an integer", async () => {
    const { settings } = await loadSettings();

    settings.notificationMinTurnSeconds.set(-5);
    expect(get(settings.notificationMinTurnSeconds)).toBe(
      settings.NOTIFICATION_MIN_TURN_SECONDS_MIN,
    );

    settings.diffContextLines.set(2.7);
    expect(get(settings.diffContextLines)).toBe(3);
  });

  it("clamps via update() too", async () => {
    const { settings } = await loadSettings();

    settings.notificationMinTurnSeconds.set(100);
    settings.notificationMinTurnSeconds.update((value) => value + 9000);
    expect(get(settings.notificationMinTurnSeconds)).toBe(
      settings.NOTIFICATION_MIN_TURN_SECONDS_MAX,
    );
  });

  it("clamps an out-of-range stored value on initial read", async () => {
    const { settings } = await loadSettings({ [MIN_TURN_KEY]: "9999" });

    expect(get(settings.notificationMinTurnSeconds)).toBe(
      settings.NOTIFICATION_MIN_TURN_SECONDS_MAX,
    );
  });

  it("falls back to the default for a non-finite write", async () => {
    const { settings } = await loadSettings();

    settings.notificationMinTurnSeconds.set(42);
    settings.notificationMinTurnSeconds.set(Number.NaN);
    expect(get(settings.notificationMinTurnSeconds)).toBe(
      settings.DEFAULT_NOTIFICATION_MIN_TURN_SECONDS,
    );
  });
});

const COMMIT_INSTRUCTIONS_KEY = "hitch.draftCommitInstructions";
const PR_INSTRUCTIONS_KEY = "hitch.draftPrInstructions";

describe("Draft Instructions persistence", () => {
  it("defaults both instruction settings to empty strings", async () => {
    const { settings } = await loadSettings();

    expect(get(settings.draftCommitInstructions)).toBe("");
    expect(get(settings.draftPrInstructions)).toBe("");
  });

  it("round-trips a written value to localStorage", async () => {
    const { storage, settings } = await loadSettings();

    settings.draftCommitInstructions.set("Use Conventional Commits.");
    settings.draftPrInstructions.set("Write in past tense.");

    expect(storage.values.get(COMMIT_INSTRUCTIONS_KEY)).toBe("Use Conventional Commits.");
    expect(storage.values.get(PR_INSTRUCTIONS_KEY)).toBe("Write in past tense.");
  });

  it("reads stored values back on init", async () => {
    const { settings } = await loadSettings({
      [COMMIT_INSTRUCTIONS_KEY]: "Reference the ticket.",
      [PR_INSTRUCTIONS_KEY]: "Include a Testing section.",
    });

    expect(get(settings.draftCommitInstructions)).toBe("Reference the ticket.");
    expect(get(settings.draftPrInstructions)).toBe("Include a Testing section.");
  });
});
