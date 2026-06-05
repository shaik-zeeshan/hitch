import type { KnownAgent } from "./types";

export type SessionTabKind = "claude" | "codex" | "shell";

const AGENT_DISPLAY: Record<KnownAgent, { kind: Exclude<SessionTabKind, "shell">; title: string }> = {
  "claude-code": { kind: "claude", title: "Claude Code" },
  codex: { kind: "codex", title: "Codex" },
};

// The Session mark (glyph + agent title) comes from the daemon-announced Agent
// identity ONLY (ADR 0011 amendment 2026-06-05): the agent's own `SessionStart`
// hook announces which agent the moment its TUI starts, so a hand-typed `claude`
// is marked the same as a launched one. Identity is never inferred from the
// session title or launch command — that fragile fallback was deleted.
//
// `agent` is the announced identity for the session (null = no known agent
// running). `command` is the live foreground command, used only to label a
// non-agent shell tab (e.g. "vim"); it never decides the agent kind.
export function sessionTabKind(agent: KnownAgent | null | undefined): SessionTabKind {
  return agent ? AGENT_DISPLAY[agent].kind : "shell";
}

export function sessionTabTitle(
  agent: KnownAgent | null | undefined,
  sessionName: string,
  command: string | null | undefined,
): string {
  if (agent) return AGENT_DISPLAY[agent].title;
  return command ?? sessionName;
}
