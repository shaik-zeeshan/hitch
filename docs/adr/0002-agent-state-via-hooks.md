# Agents run as plain CLIs; Hitch learns their state via agent hooks

Hitch does not render a chat UI for AI agents and does not run them through a structured headless harness. An agent (Claude Code, Codex, …) runs as an ordinary CLI inside a PTY Session, drawing its own TUI. To surface agent state (running / needs-approval / completed / error), Hitch writes the agent's own hook config into each managed worktree — Claude Code `Notification`/`Stop` hooks, Codex notify — so the agent reports state to Hitch over a local channel.

## Considered Options

- **Agent hooks/notify** (chosen) — reliable state from the agent itself; works even when the user launches the agent by typing `claude`, because the hook lives in the worktree config, not in how the Session was launched. Only covers agents Hitch has integration for.
- **Terminal scraping/heuristics** — infer state from raw PTY bytes (idle, bell, OSC, regex on prompts). Works for any CLI but is fragile and approximate.
- **Structured headless harness + Hitch chat UI** — run the agent in `stream-json`/SDK mode and render our own transcript. Most reliable and richest, but rebuilds the interaction the user explicitly did not want.

## Consequences

- Only known agents get Agent State; plain shells and unknown CLIs get none.
- Hitch must own the hook config inside each worktree and a local channel (file/socket) for the agent to report into.
- Adding support for a new agent means teaching Hitch that agent's hook/notify mechanism.

## Registry and hook installation

Known agents live in a **built-in registry** (Claude Code and Codex to start), each a code-level integration describing its launch command and hook mechanism. Hook integration is per-agent (the mechanisms differ structurally), so this is intentionally code, not user config; adding an agent is a contained change. Commands outside the registry still run as plain Sessions, without Agent State.

Hitch installs the hook by **merging** it into a per-worktree, gitignored agent-local config (e.g. `<worktree>/.claude/settings.local.json`) — never overwriting the user's own keys, and ensured gitignored so it is not committed. The installed hook invokes a small `hitch` helper CLI that reports state to the daemon's local socket. Because the hook lives in the worktree config rather than being injected at launch, a hand-typed `claude` reports state too. Rejected alternatives: a user-global hook scoped by path (touches global config, fires for all `claude` usage) and launch-time injection via `--settings` (cleanest filesystem but a hand-typed agent gets no state).
