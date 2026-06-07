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
- *waiting* — the Agent finished its turn and is **idle at its prompt**, content to wait; the ball is in the user's court. The soft "your turn". **Renders unlabeled** in the shell (decided 2026-06-05): a finished turn is visually just idle — *waiting* remains in the taxonomy as the hook-truth that a turn ended (it drops *running*, floors the rollup, and feeds palette/sorting), but no word, badge, or dot is shown for it, and there is nothing to dismiss.
- *error* — the Agent's **turn** failed (agent-reported via its failure hook: rate limit, billing, server error, …); the Agent process is still alive at its prompt. A process exit is never *error* — exit clears to `None`.

*needs-approval* and *error* are the **act states**: they alone demand the user's attention, and one predicate (`state ∈ {needs-approval, error}`) drives every attention surface — the row's state word, the tab needdot, and the collapsed-project rollup pill — all in the single attention color (decided 2026-06-05).

A Session not running a known Agent has **no** Agent State (`None`). This absence is also how Hitch models an Agent that has **exited** — when the Agent process leaves the Session's foreground (the user quit `claude`/`codex`, or it died), the Agent State clears to `None` rather than lingering on a stale value or becoming a distinct terminal status. There is no *completed* state: an interactive Agent in a PTY never "completes", it goes *waiting* until re-prompted or it exits to `None`.
_Avoid_: Status (too generic), Completed/Done (an interactive Agent does not terminate into a state — it idles to *waiting* or exits to `None`), Idle (resolved as *waiting*).

**Session mark**:
The glyph identifying what a **Session** runs: a known **Agent**'s mark (Claude, Codex) or the shell mark. Tabs mark every Session; the worktree row's facepile shows **Agent Sessions only** — a worktree running only shells has an empty pile (decided 2026-06-05). Mark identity is announced by the Agent's own `SessionStart` hook as an **identity-only report** (it carries which Agent, never an Agent State) and clears when the Agent exits to `None`, reverting the Session to the shell mark. Identity is never inferred from the Session's title or launch command.
_Avoid_: Harness mark (see **Agent**'s avoided terms), State glyph (marks identify what runs; they never convey Agent State).

**Agent Registry**:
Hitch's built-in set of known Agents (Claude Code, Codex to start), each with a code-level integration describing its launch command and hook mechanism. Adding an Agent is a contained code change, not user config. Commands outside the registry still run as plain Sessions, just without Agent State.
_Avoid_: Plugins, Providers.

**Hook helper**:
A small `hitch` CLI invoked by an Agent's installed hook; it reports the Agent's state to the **Daemon**'s local socket, which **owns** the current value (see ADR 0011). It is strictly **one-way**: a fire-and-forget reporter, never a decision channel. Permission prompts are the Agent's own terminal UI — Hitch surfaces *needs-approval* and routes attention to the Session, but never renders or answers the prompt itself (decided 2026-06-05). Hitch installs the hook by merging it into a per-Worktree, gitignored agent-local config (e.g. `.claude/settings.local.json`, `.codex/hooks.json`) without overwriting the user's own keys. Every installed hook carries an explicit state; the helper never infers state from payload text. It resolves to a Session by `HITCH_SESSION_ID` (injected into every PTY) only — a report that cannot be resolved is logged and dropped, never smeared onto a Worktree. The helper never breaks the Agent: any failure exits 0, unknown args are ignored, a `--session-id` flag is rejected, and `HITCH_HOOK_DEBUG` gates a diagnostic log (ADR 0002 amendment 2026-06-04).
_Avoid_: Notifier, Bridge.

**Job**:
A long-running **Daemon** operation dispatched off the per-client request loop onto a worker, so it never blocks other requests. A Job has a status lifecycle (*queued*, *running*, *succeeded*, *failed*, *cancelled*), broadcasts progress/completion as events the GUI observes by job id, and is cancellable. Job kinds today: slow git (*push*, *pull*, *fetch*, *clone*) and the **Draft Generator**; a future kind is a headless **Agent** run (run an Agent non-interactively to completion — not yet built). Fast git reads (*status*, *diff*) stay on the synchronous request/response path and are NOT Jobs. A Job is internal async plumbing surfaced as quiet progress; it is NOT the rejected user-facing "Task" work-item. Jobs are ephemeral — they live in daemon memory only and do NOT survive a daemon restart (unlike Sessions, whose PTYs are re-owned): a Job that was *running* when the daemon stopped is reported *failed* with reason "daemon restarted", and the user re-triggers.
_Avoid_: Task (a Job is not a tracked tree work-item — see Session's avoided terms), Background process (that is the Daemon itself).

**Draft Generator**:
A non-interactive generation run that drafts commit messages, commit bodies, or PR descriptions from git context. It is one kind of **Job** — dispatched off the request loop rather than blocking a synchronous request as it does today. Its provider binaries (claude/codex) are user-configurable paths, needed where they aren't on the service PATH (ADR 0007 amendment 2026-06-04).
_Avoid_: Agent harness, Agent.

**History**:
The right rail's second view (decided 2026-06-06): the selected **Worktree**'s full commit log from HEAD, switched via a CHANGES | HISTORY header toggle (one view at a time, `←`/`→` when the git pane is focused). Commits ahead of the base branch carry an iris branch-work marker; rows are two lines (summary, then sha · relative time · author). Freshness rides the existing status backbone: the HEAD commit id travels in `GitStatus`, and a changed id refetches the log (~1s, covers Agent commits made in a PTY). Selecting a commit opens its **Commit Tab**. Merge commits diff against their first parent and carry a *merge* badge.
_Avoid_: Log view (UI label is HISTORY), Timeline.

**Commit Tab**:
A center-pane diff tab showing one commit, opened from **History** — one tab **per commit**, keyed by sha (peer of file diff tabs and all-changes). Label is a commit glyph + 7-char short sha; body is a metadata header (full sha, message, author, date, ±totals) above collapsible per-file sections in the all-changes style. Its content is immutable: cached per sha with no invalidation, and the tab survives history rewrites (an amended/rebased-away sha keeps its open tab; the object is still readable).
_Avoid_: Commit view (it is a tab, not a rail view), Revision tab.

**Desktop Notification**:
A native OS notification raised by the GUI from **live** **Agent State** transitions — never from state replayed on attach (ADR 0011 replay is for catching up, not re-alerting). Three triggers (decided 2026-06-07): a Session entering *needs-approval*, entering *error*, and a **turn end** (*running* → *waiting*) gated on the turn having run at least a user-configurable minimum (default 30s); the gate's clock starts when the turn enters *running* and survives *needs-approval* pauses, so an approved-then-finished long task still notifies. Copy is agent + state in the title, `project · branch` in the body, with *error* detail appended when present; all three play the default OS sound. Click is the OS default (activate the app — no Session routing; revisit if it stings). Suppression is one user setting with three modes: *off*, *app-in-background* (notify only when the GUI is unfocused), and *background-or-other-session* (**default** — notify unless the GUI is focused AND that Session is the visible one). The GUI fires these (it owns chrome preferences, focus, and the visible Session); the **Daemon** raises none. OS permission is requested at GUI startup whenever the mode is not *off*.
_Avoid_: Completed notification (a turn ends to *waiting*; nothing "completes"), Notification (bare — collides with the agents' own `Notification` hook event), Alert.

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
- A **Desktop Notification** derives from a live **Agent State** transition observed by the GUI; the **Daemon** stores and broadcasts state but never raises notifications. Codex's missing failure hook (known gap) means *error* notifications never fire for Codex.
- **History** belongs to the right rail and shows exactly one **Worktree**'s log; a **Commit Tab** belongs to the center tab strip. Commit reads (log, commit diff) follow the fast synchronous git-read path, not **Jobs**.

## Flagged ambiguities

- "Tracked task" — there is no Task entity. A Session running an Agent is the closest thing; its "tracking" is just its Agent State, surfaced in the tree/tab. Typing `claude` in any Session reports state because the hook lives in the worktree config, not in how the Session was launched.
- "Agent harness" — resolved as **Draft Generator** for this feature; **Agent** remains reserved for known CLIs running in Sessions.
- "completed" / "done" — removed (ADR 0011). An interactive Agent does not terminate into a state: a finished turn is *waiting*, an exited Agent is `None`.
- "your turn" is two distinct states: *needs-approval* (blocking gate, sticky until resolved) vs *waiting* (idle prompt, unlabeled in the shell). The former dismiss-on-seen machinery for *waiting* is removed with the Paper Terminal shell (2026-06-05) — an unlabeled state has nothing to dismiss.
- **Known gap:** Codex exposes no failure hook, so the *error* Agent State is never shown for Codex (a crash clears to `None`); accepted for now (ADR 0011).
- **Known gap:** abandoning a permission prompt (Esc at the gate) can leave a wrong *needs-approval* for up to ~60s until the agent's idle notification self-heals it to *waiting* (ADR 0011 amendment 2026-06-05). The heal is **Claude-only** — Codex has no idle notification, so a Codex residue lasts until the user re-prompts that Session.
- **Known gap:** Codex has no `SessionEnd` hook (docs-verified 2026-06-05), so a clean Codex exit never hook-clears to `None`. On Unix, the foreground-poller backstop clears Codex identity/state when it exits back to the shell. On Windows, Hitch avoids installing the Codex `SessionStart` identity announce because there is no backstop clear path; Codex Agent State may still linger after a dirty exit until the next hook report or Session close.
- **Known gap:** Codex executes a project-local `.codex/hooks.json` only after the user trusts that project's `.codex` layer (Codex `/hooks`). Hitch installs the file but cannot auto-trust it; until trusted, Codex Agent State is silently absent. Accepted; a UX nudge may come later.
