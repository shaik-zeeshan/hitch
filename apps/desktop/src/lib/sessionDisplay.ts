import type { KnownAgent } from "./types";

export type SessionTabKind = "claude" | "codex" | "shell";

const AGENT_DISPLAY: Record<KnownAgent, { kind: Exclude<SessionTabKind, "shell">; title: string }> = {
  "claude-code": { kind: "claude", title: "Claude Code" },
  codex: { kind: "codex", title: "Codex" },
};

export function sessionTabTitle(
  sessionName: string,
  command: string | null | undefined,
  agent: KnownAgent | null | undefined,
): string {
  if (agent) return AGENT_DISPLAY[agent].title;
  return command ?? sessionName;
}

export function sessionTabKind(agent: KnownAgent | null | undefined): SessionTabKind {
  return agent ? AGENT_DISPLAY[agent].kind : "shell";
}
