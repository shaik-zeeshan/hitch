# Agents run as plain CLIs; Hitch learns their state via agent hooks

Hitch does not render a chat UI for AI agents and does not run them through a structured headless harness. An agent (Claude Code, Codex, …) runs as an ordinary CLI inside a PTY Session, drawing its own TUI. To surface agent state (running / needs-approval / completed / error), Hitch writes the agent's own hook config into each managed worktree — Claude Code lifecycle hooks and Codex lifecycle hooks — so the agent reports state to Hitch over a local channel.

## Considered Options

- **Agent hooks** (chosen) — reliable state from the agent itself; works even when the user launches the agent by typing `claude` or `codex`, because the hook lives in the worktree config, not in how the Session was launched. Only covers agents Hitch has integration for.
- **Terminal scraping/heuristics** — infer state from raw PTY bytes (idle, bell, OSC, regex on prompts). Works for any CLI but is fragile and approximate.
- **Structured headless harness + Hitch chat UI** — run the agent in `stream-json`/SDK mode and render our own transcript. Most reliable and richest, but rebuilds the interaction the user explicitly did not want.

## Consequences

- Only known agents get Agent State; plain shells and unknown CLIs get none.
- Hitch must own the hook config inside each worktree and a local channel (file/socket) for the agent to report into.
- Adding support for a new agent means teaching Hitch that agent's hook mechanism.

## Registry and hook installation

Known agents live in a **built-in registry** (Claude Code and Codex to start), each a code-level integration describing its launch command and hook mechanism. Hook integration is per-agent (the mechanisms differ structurally), so this is intentionally code, not user config; adding an agent is a contained change. Commands outside the registry still run as plain Sessions, without Agent State.

Hitch installs the hook by **merging** it into a per-worktree, gitignored agent-local config (e.g. `<worktree>/.claude/settings.local.json` for Claude Code, `<worktree>/.codex/hooks.json` for Codex) — never overwriting the user's own keys, and ensured gitignored so it is not committed. The installed hook invokes a small `hitch` helper CLI that reports state to the daemon's local socket. Because the hook lives in the worktree config rather than being injected at launch, a hand-typed `claude` or `codex` reports state too. Rejected alternatives: a user-global hook scoped by path (touches global config, fires for all agent usage) and launch-time injection (cleanest filesystem but a hand-typed agent gets no state).

## Amendment (2026-06-04): hook helper hardening

The `hitch-hook` helper must never break the agent that runs it: a non-zero exit makes Claude Code / Codex treat their own hook as failed, surfacing an error and possibly interrupting the turn. So the helper degrades every failure (malformed invocation, absent or busy daemon, transport fault) to a logged no-op with exit 0, and **ignores unrecognized arguments** rather than rejecting them — an agent may append its own context (an event name, a JSON blob, extra flags) after the configured command, and the known state-carrying flags must still take effect.

Session identity comes **only** from the `HITCH_SESSION_ID` environment variable (injected into every PTY). The previously-accepted `--session-id` flag is now explicitly **rejected** with a usage error, so a foreign, forged, or stale CLI argument can never misroute one session's state onto another — aligning with ADR 0011's hook-identity rule.

For field diagnosis, setting `HITCH_HOOK_DEBUG` (any value) appends what the hook received to a temp-dir log; it is off by default because those lines carry session IDs and socket paths.
