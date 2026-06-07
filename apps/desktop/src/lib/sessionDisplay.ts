import type { Component } from "svelte";
import Claude from "~icons/hitch/claude";
import Codex from "~icons/hitch/codex";
import Shell from "~icons/hitch/shell";
import type { KnownAgent } from "./types";

export type SessionTabKind = "claude" | "codex" | "shell";

// The Agent Registry (CONTEXT.md "Agent Registry"): Hitch's built-in set of known
// Agents, each a contained code-level integration — never user config. One entry
// per agent carries EVERYTHING the UI needs to mark and launch it: the `kind`
// (the harness mark/launch name), the human `title`, the identity mark `icon`,
// and the `launchArgv` to spawn it. Every UI surface (tab marks, the new-session
// menus, the worktree facepile, the palette) derives from this single map, so a
// new agent is added here once instead of edited into each call site.
type AgentEntry = {
  kind: Exclude<SessionTabKind, "shell">;
  title: string;
  icon: Component;
  // The argv handed to `openSession` to spawn this agent (mirrors the daemon's
  // OpenSession.command). Kept paired with `kind` so the daemon's launch name
  // and command never drift apart.
  launchArgv: string[];
};

const AGENT_DISPLAY: Record<KnownAgent, AgentEntry> = {
  "claude-code": { kind: "claude", title: "Claude Code", icon: Claude, launchArgv: ["claude"] },
  codex: { kind: "codex", title: "Codex", icon: Codex, launchArgv: ["codex"] },
};

// The harness mark per Session-tab kind — the known agents' marks plus the shell
// fallback for a Session running no known Agent. Keyed by `kind` (not the
// announced `KnownAgent`) because the tab/facepile already work in `kind` space.
export const TAB_MARK: Record<SessionTabKind, Component> = {
  claude: AGENT_DISPLAY["claude-code"].icon,
  codex: AGENT_DISPLAY.codex.icon,
  shell: Shell,
};

// A launchable agent as the new-session menus / palette need it: the display
// `title` + `icon`, and the `kind`/`launchArgv` pair fed straight to
// `openSession`. Shell is deliberately absent — it is not a known Agent and is
// offered (where offered at all) as a separate "plain shell" affordance.
export type LaunchableAgent = {
  kind: Exclude<SessionTabKind, "shell">;
  title: string;
  icon: Component;
  launchArgv: string[];
};

export const LAUNCHABLE_AGENTS: LaunchableAgent[] = Object.values(AGENT_DISPLAY).map(
  ({ kind, title, icon, launchArgv }) => ({ kind, title, icon, launchArgv }),
);

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

// Human-readable agent name for prose (e.g. notification titles "Claude Code
// finished"). Reuses the same announced-identity → title map as the Session
// mark so the two never drift; falls back to a generic "Agent" when no known
// agent is announced (the notification still fires, just without a name).
export function agentDisplayName(agent: KnownAgent | null | undefined): string {
  return agent ? AGENT_DISPLAY[agent].title : "Agent";
}
