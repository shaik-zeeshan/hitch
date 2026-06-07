import { describe, expect, it } from "vitest";
import { copyForTransition, shouldNotify } from "./notifications";

// Fixed clock + a turn that started 60s earlier, so duration gating is exact.
const NOW = 1_000_000;
const STARTED_60S_AGO = NOW - 60_000;

describe("copyForTransition", () => {
  it("titles a needs-approval transition and keeps the body", () => {
    expect(
      copyForTransition(null, "needs-approval", "Claude Code", "demo · main", null, 30, undefined, NOW),
    ).toEqual({ title: "Claude Code needs approval", body: "demo · main" });
  });

  it("appends a non-empty detail to the body on error", () => {
    expect(
      copyForTransition("running", "error", "Codex", "demo · main", "rate limited", 30, STARTED_60S_AGO, NOW),
    ).toEqual({ title: "Codex hit an error", body: "demo · main — rate limited" });
  });

  it("surfaces the error detail alone when there is no body", () => {
    expect(copyForTransition("running", "error", "Agent", null, "boom", 30, undefined, NOW)).toEqual({
      title: "Agent hit an error",
      body: "boom",
    });
  });

  it("ignores a blank error detail", () => {
    expect(copyForTransition("running", "error", "Agent", "demo · main", "   ", 30, undefined, NOW)).toEqual({
      title: "Agent hit an error",
      body: "demo · main",
    });
  });

  it("notifies a finished turn that ran at least the minimum", () => {
    expect(
      copyForTransition("running", "waiting", "Claude Code", "demo · main", null, 30, STARTED_60S_AGO, NOW),
    ).toEqual({ title: "Claude Code finished", body: "demo · main" });
  });

  it("suppresses a finished turn shorter than the minimum", () => {
    const startedRecently = NOW - 5_000; // 5s < 30s gate
    expect(
      copyForTransition("running", "waiting", "Claude Code", "demo · main", null, 30, startedRecently, NOW),
    ).toBeNull();
  });

  it("notifies a short finished turn when the gate is 0 (ungated)", () => {
    const startedRecently = NOW - 5_000;
    expect(
      copyForTransition("running", "waiting", "Claude Code", null, null, 0, startedRecently, NOW),
    ).not.toBeNull();
  });

  it("notifies a finished turn whose start was never observed (ungated fallback)", () => {
    expect(
      copyForTransition("running", "waiting", "Claude Code", "demo · main", null, 30, undefined, NOW),
    ).not.toBeNull();
  });

  it("does not notify a waiting transition that did not come from running", () => {
    expect(
      copyForTransition("needs-approval", "waiting", "Claude Code", "demo · main", null, 0, undefined, NOW),
    ).toBeNull();
  });

  it("does not notify entering running", () => {
    expect(copyForTransition("waiting", "running", "Claude Code", "demo · main", null, 0, undefined, NOW)).toBeNull();
  });
});

describe("shouldNotify suppression policy", () => {
  it("off never notifies", () => {
    expect(shouldNotify("off", "s1", false, null)).toBe(false);
    expect(shouldNotify("off", "s1", true, "s2")).toBe(false);
  });

  it("app-in-background notifies only when the window is unfocused", () => {
    expect(shouldNotify("app-in-background", "s1", false, "s1")).toBe(true);
    expect(shouldNotify("app-in-background", "s1", true, "s1")).toBe(false);
    expect(shouldNotify("app-in-background", "s1", true, "s2")).toBe(false);
  });

  it("background-or-other-session suppresses only the focused, active session", () => {
    // Focused on the very session that changed → suppress.
    expect(shouldNotify("background-or-other-session", "s1", true, "s1")).toBe(false);
    // Focused but a different session changed → notify.
    expect(shouldNotify("background-or-other-session", "s1", true, "s2")).toBe(true);
    // Backgrounded → notify regardless of which session is active.
    expect(shouldNotify("background-or-other-session", "s1", false, "s1")).toBe(true);
  });
});
