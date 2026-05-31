import type { KnownAgent } from "./types";

export type SessionTabKind = "claude" | "codex" | "shell";

const AGENT_DISPLAY: Record<KnownAgent, { kind: Exclude<SessionTabKind, "shell">; title: string }> = {
  "claude-code": { kind: "claude", title: "Claude Code" },
  codex: { kind: "codex", title: "Codex" },
};

function agentForCommand(command: string | null | undefined): KnownAgent | null {
  const normalized = (command ?? "").trim().toLowerCase();
  switch (normalized) {
    case "claude":
    case "claude-code":
    case "claude code":
      return "claude-code";
    case "codex":
      return "codex";
    default:
      return normalized.includes("claude code") ? "claude-code" : null;
  }
}

function looksLikeClaudeVersionCommand(command: string | null | undefined): boolean {
  return /^\d+(?:\.\d+)+(?:\s|$)/.test((command ?? "").trim());
}

function initialAgentForSessionName(
  sessionName: string,
  command: string | null | undefined,
): KnownAgent | null {
  if (command != null) return null;
  switch (sessionName.trim().toLowerCase()) {
    case "claude":
    case "claude-code":
    case "claude code":
      return "claude-code";
    case "codex":
      return "codex";
    default:
      return null;
  }
}

function displayAgent(sessionName: string, command: string | null | undefined): KnownAgent | null {
  const commandAgent = agentForCommand(command);
  if (commandAgent) return commandAgent;
  if (
    looksLikeClaudeVersionCommand(command) &&
    initialAgentForSessionName(sessionName, undefined) === "claude-code"
  ) {
    return "claude-code";
  }
  return initialAgentForSessionName(sessionName, command);
}

export function sessionTabTitle(sessionName: string, command: string | null | undefined): string {
  const agent = displayAgent(sessionName, command);
  if (agent) return AGENT_DISPLAY[agent].title;
  return command ?? sessionName;
}

export function sessionTabKind(
  sessionName: string,
  command: string | null | undefined,
): SessionTabKind {
  const agent = displayAgent(sessionName, command);
  return agent ? AGENT_DISPLAY[agent].kind : "shell";
}
