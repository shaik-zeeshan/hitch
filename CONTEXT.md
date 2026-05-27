# Hitch

A native desktop app for managing multiple software projects and their git worktrees, with PTY-backed terminal tabs for running commands or driving AI coding agents. Built on Tauri (Rust backend, Svelte/TypeScript frontend, SQLite state).

## Language

**Project**:
A workspace rooted at a single local directory that the user has added to Hitch. A Project is either *git-backed* (rooted at a git repository) or a *plain folder*. Git-backed Projects support worktrees and git operations; plain folders support only terminals.
_Avoid_: Repo (a Project may not be a repo), Workspace.

**Worktree**:
A git working tree belonging to a git-backed Project, checked out on exactly one branch. The original directory the user added is the *main worktree* (flagged primary); all others are *created worktrees* that Hitch places under a managed global directory (`~/.hitch/worktrees/<project>/<branch>/`). Git enforces that a branch is checked out in at most one worktree.
_Avoid_: Branch (a worktree is a checkout of a branch, not the branch itself), Checkout.

**Session**:
A single PTY (pseudo-terminal) process running in a fixed working directory — a created **Worktree** or main worktree for git-backed Projects, or the Project root for plain folders. The unit of work in Hitch. A user runs anything in it: shell commands, build tools, or an agent CLI. Sessions are owned by the **Daemon** (not the GUI), organized in the tree under their worktree/project, are nameable, and there may be several per worktree.
_Avoid_: Task (there is no separate tracked-task entity), Tab (the tab is the UI view of a Session).

**Daemon**:
The long-lived Hitch background process that owns every **Session**'s PTY, buffers its scrollback, and receives **Agent State** hook notifications. It is spawned detached by the GUI and outlives window-close and Cmd-Q (signalled by a menu-bar item), so Sessions survive app quit/reopen. It does not survive a machine reboot — across a reboot Hitch falls back to reopening the saved session layout as fresh terminals. The GUI is a thin client that connects over a local socket and *reattaches* on launch.
_Avoid_: Server, Backend.

**Agent**:
A known AI coding CLI that Hitch has integration for (e.g. Claude Code, Codex) — as opposed to an arbitrary command. When an Agent runs in a Session, Hitch surfaces its **Agent State**. An Agent is just a CLI in a Session; Hitch does not render a chat UI for it.
_Avoid_: Harness, Assistant.

**Agent State**:
The current status of an Agent running in a Session, reported by the agent's own hook system (Claude Code `Notification`/`Stop` hooks, Codex notify) over a local channel — not inferred from terminal output. Values: *running*, *needs-approval*, *completed*, *error*. A Session not running a known Agent has no Agent State.
_Avoid_: Status (too generic).

**Agent Registry**:
Hitch's built-in set of known Agents (Claude Code, Codex to start), each with a code-level integration describing its launch command and hook mechanism. Adding an Agent is a contained code change, not user config. Commands outside the registry still run as plain Sessions, just without Agent State.
_Avoid_: Plugins, Providers.

**Hook helper**:
A small `hitch` CLI invoked by an Agent's installed hook; it reports the Agent's state to the **Daemon**'s local socket. Hitch installs the hook by merging it into a per-Worktree, gitignored agent-local config (e.g. `.claude/settings.local.json`) without overwriting the user's own keys.
_Avoid_: Notifier, Bridge.

**Draft Generator**:
A non-interactive generation run that drafts commit messages, commit bodies, or PR descriptions from git context.
_Avoid_: Agent harness, Agent.

## Relationships

- A **Project** is either git-backed or a plain folder (its *kind*).
- A git-backed **Project** owns one or more **Worktrees**; exactly one is the main worktree.
- A plain-folder **Project** owns no worktrees and exposes no git operations.
- A **Worktree** is checked out on exactly one branch; a branch maps to at most one **Worktree**.
- A **Session** runs in exactly one Worktree (git-backed) or one plain-folder Project root.
- A **Session** running a known **Agent** has an **Agent State**; other Sessions do not.
- Hitch enables Agent State by writing the agent's hook config into the Worktree it manages.
- A **Draft Generator** runs outside Sessions and does not produce **Agent State**.

## Flagged ambiguities

- "Tracked task" — there is no Task entity. A Session running an Agent is the closest thing; its "tracking" is just its Agent State, surfaced in the tree/tab. Typing `claude` in any Session reports state because the hook lives in the worktree config, not in how the Session was launched.
- "Agent harness" — resolved as **Draft Generator** for this feature; **Agent** remains reserved for known CLIs running in Sessions.
