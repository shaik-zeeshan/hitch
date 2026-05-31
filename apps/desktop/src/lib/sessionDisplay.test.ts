import { describe, expect, it } from "vitest";

import { sessionTabKind, sessionTabTitle } from "./sessionDisplay";

describe("session tab display", () => {
  it("uses active foreground agent commands for Claude Code tab display", () => {
    expect(sessionTabTitle("shell", "1.0.128 (Claude Code)")).toBe("Claude Code");
    expect(sessionTabKind("shell", "claude")).toBe("claude");
  });

  it("treats Claude's version-only foreground label as Claude Code for Claude sessions", () => {
    expect(sessionTabTitle("claude", "1.0.128")).toBe("Claude Code");
    expect(sessionTabKind("claude", "1.0.128")).toBe("claude");
    expect(sessionTabTitle("shell", "1.0.128")).toBe("1.0.128");
  });

  it("uses active foreground agent commands for Codex tab display", () => {
    expect(sessionTabTitle("shell", "codex")).toBe("Codex");
    expect(sessionTabKind("shell", "codex")).toBe("codex");
  });

  it("uses launch session names only until a foreground command is known", () => {
    expect(sessionTabTitle("codex", undefined)).toBe("Codex");
    expect(sessionTabKind("claude", undefined)).toBe("claude");
    expect(sessionTabTitle("codex", "zsh")).toBe("zsh");
    expect(sessionTabKind("claude", "zsh")).toBe("shell");
  });

  it("preserves live foreground command labels for non-agent sessions", () => {
    expect(sessionTabTitle("shell", "vim")).toBe("vim");
    expect(sessionTabKind("shell", "vim")).toBe("shell");
  });
});
