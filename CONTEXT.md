# Hitch

A native desktop app for managing multiple software projects and their git worktrees, with PTY-backed terminal tabs for running commands or driving AI coding agents. Built on Tauri (Rust backend, Svelte/TypeScript frontend, SQLite state).

## Language

**Project**:
A workspace rooted at a directory on the machine where its owning **Daemon** runs. A Project is either *git-backed* (rooted at a git repository) or a *plain folder*. Git-backed Projects support worktrees and git operations; plain folders support only terminals. Whether the GUI reaches that Daemon locally or through an **SSH Host** is an attachment detail, not a different Project kind.
_Avoid_: Repo (a Project may not be a repo), Workspace, Remote Host Project (use **Remote Project** only when the GUI reaches the owning Daemon through SSH).

**SSH Host**:
A GUI-local saved OpenSSH target string through which the GUI can reach a Hitch **Daemon** running on that host; SSH auth remains non-interactive and owned by the user's OpenSSH config, ssh-agent, and known_hosts. A Hitch install makes its own machine reachable as one by self-installing the `hitch` CLI locally as a pure symlink (`~/.local/bin/hitch` → bundled `hitch-daemon`, plus `~/.local/bin/hitch-hook` for hook adjacency) with **no shell-rc edits**; the Hitch client then reaches that self-installed host with **zero config** by learning the daemon's absolute path from the `Hello` handshake and invoking it directly (Approach C). Making bare `ssh <host> hitch` work for a *human* against a self-installed host is out of scope; a *manual* host just needs a compatible `hitch` on its own PATH. This is local-only and never uploads binaries to a remote (ADR 0014 amendment).
_Avoid_: Server (reserved as an avoided synonym for **Daemon**), Machine, Box.

**Remote Project**:
A Project owned by a **Daemon** that the GUI reaches through an **SSH Host**.
_Avoid_: Remote Host Project, SSH Project.

**Worktree**:
A git working tree belonging to a git-backed Project, checked out on exactly one branch. The original directory the user added is the *main worktree* (flagged primary); all others are *created worktrees* that Hitch places under a managed global directory (`~/.hitch/worktrees/<project>/<branch>/`). Git enforces that a branch is checked out in at most one worktree.
_Avoid_: Branch (a worktree is a checkout of a branch, not the branch itself), Checkout.

**Session**:
A single PTY (pseudo-terminal) process running in a fixed working directory on the machine where its owning **Daemon** runs — a created **Worktree** or main worktree for git-backed Projects, or the Project root for plain folders. The unit of work in Hitch. A user runs anything in it: shell commands, build tools, or an agent CLI. Sessions are owned by the **Daemon** (not the GUI), organized in the tree under their worktree/project, are nameable, and there may be several per worktree/project.
_Avoid_: Task (there is no separate tracked-task entity), Tab (the tab is the UI view of a Session), SSH Session (SSH changes how the GUI reaches the owning Daemon, not the entity).

**Daemon**:
The long-lived Hitch background process that owns every **Session**'s PTY, buffers its scrollback, and receives **Agent State** hook notifications. It is also the sole owner of git operations (status, diff, commit, push, pull, PR, worktree create/remove) and the **Worktree**/**Project** registry: a Worktree is a registry row plus its live Sessions plus a git checkout, so worktree lifecycle is one transaction the daemon must own end-to-end (e.g. removing a worktree kills its Sessions *and* runs `git worktree remove` together), and a single daemon-side poller broadcasts dirty/branch state identically to every attached GUI window. It is spawned detached by the GUI and outlives window-close and Cmd-Q (signalled by a menu-bar item), so Sessions survive app quit/reopen. It does not survive a machine reboot — across a reboot Hitch falls back to reopening the saved session layout as fresh terminals. The GUI is a thin client that connects over a local socket and *reattaches* on launch.
_Avoid_: Server, Backend.

**Daemon Status**:
The liveness/health of one **Daemon** process as observed by the GUI, distinct from any one request/response. Values: *starting* (spawn/connect issued, endpoint not yet up), *running* (a healthy daemon is listening and this GUI is attached), *unreachable* (local endpoint or SSH Host cannot currently reach a live daemon), *failed* (spawn/connect/startup errored, carrying a captured reason). Remote Daemon Status auto-reconnects with backoff for enabled SSH Hosts; that host's Project rows may be greyed as stale while unreachable, then replayed on attach. A *failed* status always carries a human-readable reason sourced from the daemon/proxy path.
_Avoid_: Connection (that is the per-GUI socket link, a narrower thing), Health (too generic).

**ssh-agent relay**:
How a remote **Daemon** authenticates git network ops (push/pull/fetch/clone) with the local user's SSH keys without those keys ever leaving the user's machine — needed because the persistent detached Daemon has no usable forwarded agent of its own. Hitch tunnels the **ssh-agent wire protocol** transparently over its own GUI↔Daemon control channel (through the SSH stdio proxy, which stays a pure bridge): the Daemon hosts **one stable ssh-agent socket** — a single fixed path, bound while any relay-capable GUI is connected and living for the Daemon's lifetime, *not* a per-connection socket that dies on reconnect — that git's `SSH_AUTH_SOCK` points at, and every byte that socket sees is relayed to the **driving GUI**, which speaks to the user's *local* ssh-agent (e.g. 1Password). A sign request therefore prompts on the *local* machine, never on the remote. The one socket serves **all** remote git that signs: the structured button ops, git **typed in a Session's terminal**, and git **run by an Agent** — the Daemon injects its path as `SSH_AUTH_SOCK` into every Session's PTY at spawn so in-terminal and Agent-run git both find it. Unlike the structured ops, terminal PTYs are left **interactive** (no forced `BatchMode`/`GIT_TERMINAL_PROMPT=0`), so a human can answer a host-key/passphrase prompt at the terminal. This supersedes the earlier OS-level `ForwardAgent` approach, which could not reach the long-lived Daemon (ADR 0014 amendments). The relayed thing is the **SSH agent** — deliberately *not* the AI **Agent**; code names it `SshAgent*` to keep the two apart.
_Avoid_: Agent forwarding (bare — collides with **Agent**; say *ssh-agent* explicitly), Key forwarding (no key is forwarded, only signatures), Agent socket (ambiguous with **Agent**), per-connection relay socket (the socket is now Daemon-stable — superseded shape).

**driving GUI**:
The relay-capable GUI a given **ssh-agent relay** sign is routed to — the machine the user is currently driving from. Chosen by **presence routing**: resolved fresh *per sign* (never frozen at terminal-open) as the most-recently-active relay-capable connection, where *activity* is connecting, typing into a **Session**, triggering a git button op, or the GUI gaining OS focus. Because the user drives one machine at a time and (in the supported single-signer setup) every machine holds the **same keys**, mis-resolving when two GUIs are connected is a *convenience* question — which screen Touch ID pops on — not a *correctness* one: the signature is valid from either. The only real cost of a wrong guess is a prompt landing on a connected-but-unattended machine. With **no** relay-capable GUI connected, a sign fails fast (`Permission denied`) rather than prompting on the remote.
_Avoid_: owner / signer identity (no persistent identity is baked into a terminal — routing is by live presence), active Session (the driving GUI is a *connection*, not a **Session**).

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
A long-running **Daemon** operation dispatched off the per-client request loop onto a worker, so it never blocks other requests. A Job has a status lifecycle (*queued*, *running*, *succeeded*, *failed*, *cancelled*), broadcasts progress/completion as events the GUI observes by job id, and is cancellable. Job kinds today: slow git (*push*, *pull*, *fetch*, *clone*) and the **Draft Generator**; planned composite kinds are *commit-and-push* and *create-pr* — daemon-executed chains whose steps (stage, draft, commit, push, create draft PR) are reported as progress events, so the chain survives the GUI navigating away or quitting, and an attaching GUI can query a worktree's active Jobs to restore the **Composer** display (decided 2026-06-07). A future kind is a headless **Agent** run (run an Agent non-interactively to completion — not yet built). Fast git reads (*status*, *diff*) stay on the synchronous request/response path and are NOT Jobs. A Job is internal async plumbing surfaced as quiet progress; it is NOT the rejected user-facing "Task" work-item. Jobs are ephemeral — they live in daemon memory only and do NOT survive a daemon restart (unlike Sessions, whose PTYs are re-owned): a Job that was *running* when the daemon stopped is reported *failed* with reason "daemon restarted", and the user re-triggers.
_Avoid_: Task (a Job is not a tracked tree work-item — see Session's avoided terms), Background process (that is the Daemon itself).

**Draft Generator**:
A non-interactive generation run that drafts commit messages, commit bodies, or PR descriptions from git context. It is one kind of **Job** — dispatched off the request loop rather than blocking a synchronous request as it does today. Its provider binaries (claude/codex) are user-configurable paths, needed where they aren't on the service PATH (ADR 0007 amendment 2026-06-04). It accepts optional **Draft Instructions**; its built-in prompt (with the JSON output contract) is never user-replaceable.
_Avoid_: Agent harness, Agent.

**Draft Instructions**:
User-written guidance (style, conventions, language) for the **Draft Generator**, kept as two app-global settings — one for commit messages, one for PR descriptions — and sent per request in the draft settings. Instructions are **appended** into the built-in prompt as an "additional instructions" block; they never replace the prompt or its JSON output contract (decided 2026-06-07). The stub provider ignores them (it is deterministic).
_Avoid_: Custom prompt (implies template replacement, which is rejected), Prompt template.

**Composer**:
The right rail's inline card for the smart action's generate-and-confirm steps, replacing the former commit/PR modal dialogs (decided 2026-06-07). It renders as an **anchored overlay** — floating over the file list just below the action button, no backdrop, Esc dismisses — so the rail never reflows and the center panes stay visible. Two modes:
- *commit* — glance-and-confirm: opening it starts a **Draft Generator** run immediately (staged files if any are staged; otherwise it stages all first), the draft fills an editable subject/body, Enter commits. Enter while generation is in flight **queues commit-on-arrival** (commits the draft the moment it lands; Esc cancels the queue; a failed generation cancels the queue, never auto-commits a fallback).
- *PR pre-flight* — a base-branch select (prefilled with the default base) plus confirm; after confirm the flow is hands-off: push → generate title/body → create the PR **as a GitHub draft** → open it in the browser. Title/body are never reviewed locally; the draft flag on GitHub is the safety net. The card stays as a progress/result display.
The existing auto-commit-push toggle bypasses the Composer's card entirely; its progress shows as the action button morphing its label in place through the chain's steps (staging → drafting → committing → pushing → subject shown briefly), never opening the body (decided 2026-06-07).
Both autonomous chains (auto commit & push, PR create) run as daemon-owned **composite Jobs**, not GUI-orchestrated sequences — the chain completes even if the user navigates away, switches worktrees, or quits the app, and a (re)attaching GUI queries the worktree's active Jobs to restore the button/card state (decided 2026-06-07). A generation failure aborts the chain before commit — a fallback message is never auto-committed.
_Avoid_: Dialog/Modal (it is anchored and non-blocking), Popover (UI-library term, not domain language).

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

- A GUI window may attach to multiple **Daemons** at once: the local Daemon plus zero or more remote Daemons reached through **SSH Hosts**.
- An **SSH Host** is saved by the GUI as attachment configuration; it is not owned by the local Daemon or by any remote Daemon.
- A **Project** is owned by exactly one **Daemon**.
- A **Remote Project** is a Project whose owning **Daemon** is reached through an **SSH Host**.
- A **Project** is either git-backed or a plain folder (its *kind*), independent of whether its owning Daemon is local or remote.
- A git-backed **Project** owns one or more **Worktrees**; exactly one is the main worktree.
- A plain-folder **Project** owns no worktrees and exposes no git operations.
- A **Worktree** is checked out on exactly one branch; a branch maps to at most one **Worktree** within its owning Daemon.
- A **Session** runs in exactly one Worktree (git-backed) or one plain-folder Project root, on the owning Daemon's machine.
- A **Session** running a known **Agent** has an **Agent State**; other Sessions do not. The **Daemon** owns the current value per Session and replays it on attach; a **Worktree**/**Project** row badge is a *derived* rollup of its Sessions' states, prioritised *needs-approval > error > waiting > running*. Global attention surfaces include Agent State from every connected Daemon, scoped with the local/SSH Host name.
- Hitch enables Agent State by writing the agent's hook config into the Worktree it manages; for a Remote Project, the remote Daemon writes the hook config and receives hook reports on the remote host.
- All remote git that signs — structured button ops, git typed in a **Session**'s terminal, and git run by an **Agent** — authenticates through the one **ssh-agent relay** socket, routed *per sign* to the **driving GUI**. The relay is remote-only: a local **Daemon** never hosts it (its terminals inherit the real agent env).
- A **Draft Generator** runs outside Sessions and does not produce **Agent State**.
- The **Composer** is the only GUI surface that triggers the **Draft Generator**; it observes the run as a **Job** and is the cancel affordance for it.
- **Draft Instructions** are app-global settings carried per draft request; they shape the **Draft Generator**'s prompt but never own it.
- A **Job** is owned by the **Daemon**, runs off the request loop, and reports its lifecycle via events; the GUI observes Jobs but never owns them. Fast git reads are not Jobs.
- **Daemon Status** describes one Daemon process's liveness; it is broader than, and contains, any single GUI's connection state. The multi-daemon tree is flat — there are no top-level local/SSH-Host scope rows; Projects from every attached Daemon hang directly off the tree root (Local Projects first, then each host's Projects grouped together, hosts alphabetical by target). A **Remote Project** carries its owning scope inline as a cloud glyph plus the dim SSH Host target on the Project row, and the host's Daemon Status (plus its Retry/Remove actions, on the Remote Project context menu) rides those rows; a Local Project shows no such badge.
- A **Desktop Notification** derives from a live **Agent State** transition observed by the GUI; the **Daemon** stores and broadcasts state but never raises notifications. Codex's missing failure hook (known gap) means *error* notifications never fire for Codex.
- **History** belongs to the right rail and shows exactly one **Worktree**'s log; a **Commit Tab** belongs to the center tab strip. Commit reads (log, commit diff) follow the fast synchronous git-read path, not **Jobs**.

## Flagged ambiguities

- "SSH support" — resolved as connecting the GUI to a **Daemon** running on an **SSH Host** through an SSH stdio proxy (ADR 0014). Sessions, git operations, Worktrees, Jobs, and Agent State remain Daemon-owned and run on that host; SSH is the transport to the remote Daemon, not a local-Daemon remote-shell feature.
- **Single-signer assumption** — the **ssh-agent relay**'s "routing is convenience, not correctness" property holds *only* because one person drives the supported setup with the **same keys** in each machine's agent. A multi-user remote **Daemon**, or different keys per machine, would make **presence routing** a correctness concern (signing with the wrong identity, or routing to someone else's agent); that world is explicitly out of scope today.
- "Tracked task" — there is no Task entity. A Session running an Agent is the closest thing; its "tracking" is just its Agent State, surfaced in the tree/tab. Typing `claude` in any Session reports state because the hook lives in the worktree config, not in how the Session was launched.
- "Agent harness" — resolved as **Draft Generator** for this feature; **Agent** remains reserved for known CLIs running in Sessions.
- "completed" / "done" — removed (ADR 0011). An interactive Agent does not terminate into a state: a finished turn is *waiting*, an exited Agent is `None`.
- "your turn" is two distinct states: *needs-approval* (blocking gate, sticky until resolved) vs *waiting* (idle prompt, unlabeled in the shell). The former dismiss-on-seen machinery for *waiting* is removed with the Paper Terminal shell (2026-06-05) — an unlabeled state has nothing to dismiss.
- **Known gap:** Codex exposes no failure hook, so the *error* Agent State is never shown for Codex (a crash clears to `None`); accepted for now (ADR 0011).
- **Known gap:** abandoning a permission prompt (Esc at the gate) can leave a wrong *needs-approval* for up to ~60s until the agent's idle notification self-heals it to *waiting* (ADR 0011 amendment 2026-06-05). The heal is **Claude-only** — Codex has no idle notification, so a Codex residue lasts until the user re-prompts that Session.
- **Known gap:** Codex has no `SessionEnd` hook (docs-verified 2026-06-05), so a clean Codex exit never hook-clears to `None`. On Unix, the foreground-poller backstop clears Codex identity/state when it exits back to the shell. On Windows, Hitch avoids installing the Codex `SessionStart` identity announce because there is no backstop clear path; Codex Agent State may still linger after a dirty exit until the next hook report or Session close.
- **Known gap:** Codex executes a project-local `.codex/hooks.json` only after the user trusts that project's `.codex` layer (Codex `/hooks`). Hitch installs the file but cannot auto-trust it; until trusted, Codex Agent State is silently absent. Accepted; a UX nudge may come later.
