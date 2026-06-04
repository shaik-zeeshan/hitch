# The Daemon owns Agent State; lifecycle, taxonomy, and resolution

ADR 0002 established that Agent State comes from an agent's own hooks, never from
scraping terminal output. This ADR refines *what the states are*, *who owns the
current value*, and *how a hook report resolves to a session* — settling the
flow that 0002 left implicit.

## What we decided

**Taxonomy.** The states are `running`, `needs-approval`, `waiting`, `error`,
plus the absence of state (`None`). There is no `completed`: an interactive agent
in a PTY never terminates into a state — when its turn ends it goes `waiting`
("your turn"), and when the agent process exits it clears to `None`.
`needs-approval` is the *blocking* "your turn" (stuck on a permission gate);
`waiting` is the *soft* "your turn" (idle at its prompt, content to wait).

**Lifecycle (event → state).** `UserPromptSubmit`/`PostToolUse` → `running`;
`PermissionRequest`/`Notification`(permission) → `needs-approval`; `Stop` →
`waiting`; `StopFailure` (Claude only) → `error`; `SessionEnd` → `None`. A fresh,
never-prompted agent has **no** state — `SessionStart` is intentionally *not*
wired, because there is nothing to "wait" on until the first prompt. Adding
`PostToolUse` → `running` closes the approval gap: a tool only runs *after*
approval, so it reliably moves `needs-approval` → `running` (no `PreToolUse`,
which would flicker `running` right before the gate).

**The Daemon owns the current value.** Each daemon session record holds
`Option<AgentState>` (+ detail + which agent). It is included in `SessionOpened`
/ replay, so every attaching GUI window is immediately correct and consistent —
fixing today's bug where a late-joining window is blind to in-flight state. The
GUI becomes a pure renderer; worktree/project row badges are *derived* rollups of
their sessions' states, not separately stored. "Seen"/dismissal stays GUI-local
(two windows can independently have seen a state).

**Exit clears to `None`** via the agent's own `SessionEnd` (instant, clean exit),
with the daemon's foreground-command poller as a backstop for dirty exits
(`kill -9`, crash, terminal close) where `SessionEnd` cannot fire.

**Resolution is by session id only.** `HITCH_SESSION_ID` is injected into every
PTY and inherited by the agent → the hook, and is robust to `cd`. A report that
cannot resolve to a session is **logged and dropped**, never smeared across a
worktree — so two agents in one worktree never collide, and a missing badge is
debuggable. The wire carries `state: Option<AgentState>` (`null` = clear).

**No text inference.** `infer_state_from_text` (keyword-scanning the payload for
"error"/"approval"/…) is deleted: it contradicted ADR 0002 and could misfire on
an assistant message that merely mentions "error". Every installed hook passes an
explicit `--state`; a report with neither `--state` nor a known event is dropped.

**Rollup priority** for a multi-session row: `needs-approval > error > waiting >
running` — surface what needs the user before what does not.

**Dismissal semantics:** `needs-approval` is sticky-until-resolved (looking at it
does not unblock it); `waiting` and `error` are dismiss-on-seen.

## Consequences

- Proto and the daemon `Session` record change (nullable Agent State; replayed on
  attach). This is the hard-to-reverse part.
- Codex has no failure hook, so a Codex crash clears to `None` and the `error`
  state is never shown for Codex — an accepted gap for now.
- Agent State is ephemeral daemon memory; it does not survive a daemon restart,
  consistent with Sessions and Jobs.

## Windows note — no foreground-command backstop

The dirty-exit backstop above (the daemon's foreground-command poller clearing
Agent State when an agent dies without firing `SessionEnd`) is **unavailable on
Windows**. It relies on resolving the PTY's foreground process group leader, but
the Windows PTY backend is ConPTY (ADR 0012), which exposes no foreground
process group — `hitch_pty::ManagedPty::foreground_command()` returns `None`
unconditionally there. We **document this gap rather than implement** an
equivalent: ConPTY offers no reliable, supported API for the currently-focused
child, and the cases the poller covers are already largely handled elsewhere.

Consequently, on Windows Agent State relies on (a) the agent's own hooks —
notably `SessionEnd` for a clean exit — and (b) session-exit cleanup, which
clears state to `None` when the PTY/session itself goes away (including the
Job-Object tree-kill on Session close, ADR 0012). The uncovered residue is a
dirty agent-process exit that leaves the surrounding shell session alive; on
Windows such state may linger until the next hook report or until the session
closes. This is an accepted limitation, consistent with the Codex `error`-state
gap already noted above.
