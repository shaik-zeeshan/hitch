import { describe, expect, it } from "vitest";

import {
  LAUNCHABLE_AGENTS,
  TAB_MARK,
  sessionTabKind,
  sessionTabTitle,
} from "./sessionDisplay";

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

describe("agent registry", () => {
  it("carries a tab mark for every session kind, including the shell fallback", () => {
    for (const kind of ["claude", "codex", "shell"] as const) {
      expect(TAB_MARK[kind]).toBeTruthy();
    }
  });

  it("exposes the known agents as launchable, each with title, kind, icon, and argv", () => {
    const kinds = LAUNCHABLE_AGENTS.map((a) => a.kind);
    expect(kinds).toEqual(["claude", "codex"]);
    // Shell is not a known Agent — it is offered as a separate plain-shell
    // affordance, never through the launchable-agent iteration.
    expect(kinds).not.toContain("shell");

    for (const agent of LAUNCHABLE_AGENTS) {
      expect(agent.title.length).toBeGreaterThan(0);
      expect(agent.icon).toBeTruthy();
      // The launch argv stays paired with the kind so the daemon's launch name
      // and command never drift apart.
      expect(agent.launchArgv.length).toBeGreaterThan(0);
    }

    const claude = LAUNCHABLE_AGENTS.find((a) => a.kind === "claude");
    expect(claude?.title).toBe("Claude Code");
    expect(claude?.launchArgv).toEqual(["claude"]);
    const codex = LAUNCHABLE_AGENTS.find((a) => a.kind === "codex");
    expect(codex?.title).toBe("Codex");
    expect(codex?.launchArgv).toEqual(["codex"]);
  });
});
