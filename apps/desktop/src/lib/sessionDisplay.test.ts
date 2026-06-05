import { describe, expect, it } from "vitest";

import { sessionTabKind, sessionTabTitle } from "./sessionDisplay";

describe("session tab display", () => {
  it("marks the tab from the daemon-announced agent identity", () => {
    expect(sessionTabKind("claude-code")).toBe("claude");
    expect(sessionTabTitle("claude-code", "shell", "1.0.128 (Claude Code)")).toBe("Claude Code");
    expect(sessionTabKind("codex")).toBe("codex");
    expect(sessionTabTitle("codex", "shell", "codex")).toBe("Codex");
  });

  it("falls back to the shell mark with no announced agent — never inferring from name/command", () => {
    // A hand-typed `claude` whose announce hasn't arrived (or never will, e.g.
    // an untrusted Codex project) is a plain shell until identity is announced.
    expect(sessionTabKind(null)).toBe("shell");
    expect(sessionTabKind(undefined)).toBe("shell");
    expect(sessionTabTitle(null, "claude", "claude")).toBe("claude");
    expect(sessionTabTitle(null, "codex", undefined)).toBe("codex");
  });

  it("labels a non-agent tab with the live foreground command, else the session name", () => {
    expect(sessionTabTitle(null, "shell", "vim")).toBe("vim");
    expect(sessionTabTitle(null, "shell", null)).toBe("shell");
  });
});
