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
The long-lived Hitch background process that owns every **Session**'s PTY, buffers its scrollback, and receives **Agent State** hook notifications. It is also the sole owner of git operations (status, diff, commit, push, pull, PR, worktree create/remove) and the **Worktree**/**Project** registry: a Worktree is a registry row plus its live Sessions plus a git checkout, so worktree lifecycle is one transaction the daemon must own end-to-end (e.g. removing a worktree kills its Sessions *and* runs `git worktree remove` together), and a single daemon-side poller broadcasts dirty/branch state identically to every attached GUI window. It is spawned detached by the GUI and outlives window-close and Cmd-Q (signalled by a menu-bar item), so Sessions survive app quit/reopen. It does not survive a machine reboot — across a reboot Hitch falls back to reopening the saved session layout as fresh terminals. The GUI is a thin client that connects over a local socket and *reattaches* on launch.
_Avoid_: Server, Backend.

**Daemon Status**:
The liveness/health of the **Daemon** process itself, distinct from any one GUI's socket attachment. Values: *starting* (spawn issued, socket not yet up), *running* (a healthy daemon is listening and this GUI is attached), *unreachable* (no socket and no live daemon process found — it died or never ran), *failed* (spawn or startup errored, carrying a captured reason). "Is this GUI attached to the running daemon" is a sub-state of *running*, surfaced as the connection indicator. A *failed* status always carries a human-readable reason sourced from the daemon's own log.
_Avoid_: Connection (that is the per-GUI socket link, a narrower thing), Health (too generic).

**Agent**:
A known AI coding CLI that Hitch has integration for (e.g. Claude Code, Codex) — as opposed to an arbitrary command. When an Agent runs in a Session, Hitch surfaces its **Agent State**. An Agent is just a CLI in a Session; Hitch does not render a chat UI for it.
_Avoid_: Harness, Assistant.

**Agent State**:
The current status of an Agent running in a Session, reported by the agent's own hook system (Claude Code lifecycle hooks, Codex lifecycle hooks) over a local channel — not inferred from terminal output. Values:
- *running* — the Agent is actively working.
- *needs-approval* — the Agent is **blocked mid-turn** on a permission gate; it cannot proceed without the user. The urgent "your turn".
- *waiting* — the Agent finished its turn and is **idle at its prompt**, content to wait; the ball is in the user's court. The soft "your turn". (Surfaced to a person glancing at a branch as *"your turn"*.)
- *error* — the Agent stopped because of a failure.

A Session not running a known Agent has **no** Agent State (`None`). This absence is also how Hitch models an Agent that has **exited** — when the Agent process leaves the Session's foreground (the user quit `claude`/`codex`, or it died), the Agent State clears to `None` rather than lingering on a stale value or becoming a distinct terminal status. There is no *completed* state: an interactive Agent in a PTY never "completes", it goes *waiting* until re-prompted or it exits to `None`.
_Avoid_: Status (too generic), Completed/Done (an interactive Agent does not terminate into a state — it idles to *waiting* or exits to `None`), Idle (resolved as *waiting*).

**Agent Registry**:
Hitch's built-in set of known Agents (Claude Code, Codex to start), each with a code-level integration describing its launch command and hook mechanism. Adding an Agent is a contained code change, not user config. Commands outside the registry still run as plain Sessions, just without Agent State.
_Avoid_: Plugins, Providers.

**Hook helper**:
A small `hitch` CLI invoked by an Agent's installed hook; it reports the Agent's state to the **Daemon**'s local socket, which **owns** the current value (see ADR 0011). Hitch installs the hook by merging it into a per-Worktree, gitignored agent-local config (e.g. `.claude/settings.local.json`, `.codex/hooks.json`) without overwriting the user's own keys. Every installed hook carries an explicit state; the helper never infers state from payload text. It resolves to a Session by `HITCH_SESSION_ID` (injected into every PTY) only — a report that cannot be resolved is logged and dropped, never smeared onto a Worktree. The helper never breaks the Agent: any failure exits 0, unknown args are ignored, a `--session-id` flag is rejected, and `HITCH_HOOK_DEBUG` gates a diagnostic log (ADR 0002 amendment 2026-06-04).
_Avoid_: Notifier, Bridge.

**Job**:
A long-running **Daemon** operation dispatched off the per-client request loop onto a worker, so it never blocks other requests. A Job has a status lifecycle (*queued*, *running*, *succeeded*, *failed*, *cancelled*), broadcasts progress/completion as events the GUI observes by job id, and is cancellable. Job kinds today: slow git (*push*, *pull*, *fetch*, *clone*) and the **Draft Generator**; a future kind is a headless **Agent** run (run an Agent non-interactively to completion — not yet built). Fast git reads (*status*, *diff*) stay on the synchronous request/response path and are NOT Jobs. A Job is internal async plumbing surfaced as quiet progress; it is NOT the rejected user-facing "Task" work-item. Jobs are ephemeral — they live in daemon memory only and do NOT survive a daemon restart (unlike Sessions, whose PTYs are re-owned): a Job that was *running* when the daemon stopped is reported *failed* with reason "daemon restarted", and the user re-triggers.
_Avoid_: Task (a Job is not a tracked tree work-item — see Session's avoided terms), Background process (that is the Daemon itself).

**Draft Generator**:
A non-interactive generation run that drafts commit messages, commit bodies, or PR descriptions from git context. It is one kind of **Job** — dispatched off the request loop rather than blocking a synchronous request as it does today. Its provider binaries (claude/codex) are user-configurable paths, needed where they aren't on the service PATH (ADR 0007 amendment 2026-06-04).
_Avoid_: Agent harness, Agent.

## Relationships

- A **Project** is either git-backed or a plain folder (its *kind*).
- A git-backed **Project** owns one or more **Worktrees**; exactly one is the main worktree.
- A plain-folder **Project** owns no worktrees and exposes no git operations.
- A **Worktree** is checked out on exactly one branch; a branch maps to at most one **Worktree**.
- A **Session** runs in exactly one Worktree (git-backed) or one plain-folder Project root.
- A **Session** running a known **Agent** has an **Agent State**; other Sessions do not. The **Daemon** owns the current value per Session and replays it on attach; a **Worktree**/**Project** row badge is a *derived* rollup of its Sessions' states, prioritised *needs-approval > error > waiting > running*.
- Hitch enables Agent State by writing the agent's hook config into the Worktree it manages.
- A **Draft Generator** runs outside Sessions and does not produce **Agent State**.
- A **Job** is owned by the **Daemon**, runs off the request loop, and reports its lifecycle via events; the GUI observes Jobs but never owns them. Fast git reads are not Jobs.
- **Daemon Status** describes the Daemon process's own liveness; it is broader than, and contains, any single GUI's connection state.

## Flagged ambiguities

- "Tracked task" — there is no Task entity. A Session running an Agent is the closest thing; its "tracking" is just its Agent State, surfaced in the tree/tab. Typing `claude` in any Session reports state because the hook lives in the worktree config, not in how the Session was launched.
- "Agent harness" — resolved as **Draft Generator** for this feature; **Agent** remains reserved for known CLIs running in Sessions.
- "completed" / "done" — removed (ADR 0011). An interactive Agent does not terminate into a state: a finished turn is *waiting*, an exited Agent is `None`.
- "your turn" is two distinct states: *needs-approval* (blocking gate, sticky until resolved) vs *waiting* (idle prompt, dismiss-on-seen).
- **Known gap:** Codex exposes no failure hook, so the *error* Agent State is never shown for Codex (a crash clears to `None`); accepted for now (ADR 0011).
