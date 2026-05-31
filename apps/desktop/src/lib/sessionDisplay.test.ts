import { describe, expect, it } from "vitest";

import { sessionTabKind, sessionTabTitle } from "./sessionDisplay";

describe("session tab display", () => {
  it("uses the hook-reported agent for Claude Code tab display", () => {
    expect(sessionTabTitle("shell", "1.0.128 (Claude Code)", "claude-code")).toBe("Claude Code");
    expect(sessionTabKind("claude-code")).toBe("claude");
  });

  it("uses the hook-reported agent for Codex tab display", () => {
    expect(sessionTabTitle("shell", "node", "codex")).toBe("Codex");
    expect(sessionTabKind("codex")).toBe("codex");
  });

  it("does not infer agents from session names or foreground commands", () => {
    expect(sessionTabTitle("claude", "node", null)).toBe("node");
    expect(sessionTabKind(null)).toBe("shell");
    expect(sessionTabTitle("shell", "codex", undefined)).toBe("codex");
    expect(sessionTabKind(undefined)).toBe("shell");
  });

  it("preserves live foreground command labels for non-agent sessions", () => {
    expect(sessionTabTitle("shell", "vim", null)).toBe("vim");
    expect(sessionTabKind(null)).toBe("shell");
  });
});
