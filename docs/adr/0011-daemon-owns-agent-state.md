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

## Amendment (2026-06-05): the WORKING display word is output-gated

The Paper Terminal shell renders a `WORKING` word on a worktree row "while
streaming". Hook state alone cannot drive that word honestly: **Claude Code's
`Stop` hook does not fire on user interrupt (Esc)** — docs-confirmed — and there
is no interrupt hook to install, so an Esc mid-turn leaves hook state at
`running` indefinitely. A rail that says `WORKING` over an idle agent erodes the
same trust `AWAITING` depends on.

So the displayed word is gated on output activity: **`WORKING` = hook state
`running` ∧ the session's PTY produced output within the last ~N seconds** (N
tuned at implementation; agent TUIs repaint spinners continuously, so genuine
work holds the gate open). The daemon computes this — it sees every output frame
for every session regardless of GUI attachment — and broadcasts edge-triggered
output-active transitions, not timestamps. The interrupt case self-heals: output
stops, the word drops, and the row reads idle, which is exactly the design's
accepted "interrupted / hung / finished all look idle" trade-off.

This does **not** revisit the no-text-inference rule above: the gate watches
*whether* bytes flow, never what they say, and it only ever *narrows* the
display of hook-reported `running` — it cannot invent a state. Hook state stays
the stored truth (`running` until `Stop`/`SessionEnd`); the gate is display
derivation, applied by the daemon so every window agrees.

**Lifecycle additions (same date), closing the abandoned-prompt leak.** Esc or
manual deny *at a permission prompt* fires neither `PostToolUse`, `Stop`, nor
`PermissionDenied`; Claude Code documents `PermissionDenied` as auto-mode
classifier denial only, not manual permission-dialog denial/Esc. Manual
abandonment therefore leaves `needs-approval` — the sticky, saturated signal —
stale until `Notification` matcher `idle_prompt` → `waiting` fires (the agent's
own "done and waiting for input" signal, ~60s after input goes idle), which
makes the stale prompt self-heal. `PermissionDenied` → `running` is still wired,
but only for its documented scope: an auto-mode classifier denial has already
occurred, the agent consumes that denial, and the turn continues or finishes.
The existing late-arrival guard (non-`running` reports are dropped until the
first `running`) keeps a fresh, never-prompted agent at no-state even if an idle
notification fires. Accepted residue: an abandoned prompt can show a wrong
`AWAITING` for up to ~60s before the idle notification heals it.

**`waiting` stays in the taxonomy but renders unlabeled; dismissal machinery is
removed.** The Paper Terminal shell (2026-06-05) labels only the act/inform
states (`AWAITING`/`ERROR`/`WORKING`); a finished turn is visually idle. The
"dismissal semantics" above are superseded: with no `waiting` badge there is
nothing to dismiss, so dismiss-on-seen (GUI-local seen-state, auto-dismiss
timers) is deleted rather than ported. `needs-approval` remains sticky by
construction (the word shows whenever the state holds).

**`error` outranks the idle heal.** The `idle_prompt` → `waiting` heal applies
only over `running` / `needs-approval`. After `StopFailure` the agent is also
idle at its prompt; letting the ~60s idle notification overwrite `error` would
silently blank an unseen failure. `error` holds until the user re-prompts
(`UserPromptSubmit` → `running`) or the agent exits to `None`. This is a
transition-precedence rule on the daemon-owned value, not inference.

**The rollup shows on every worktree row, including the selected/actively-watched one.** The prior shell suppressed the `running` word on the worktree you were live-viewing (rationale: "you can see that agent live in the main pane"). The shell redesign (commit 139d349) removed that guard so the rollup is a **pure derivation of session states**, with no dependence on selection or focus — the row reads the same whether or not it is the one you are watching. This keeps the rollup window-agnostic and matches the daemon-owned, replay-on-attach model: display follows state, not the GUI's current view.

**`SessionStart` is wired after all — but as identity, not state.** The "not
wired" rule above was about *state*, and stands: a fresh agent still has no
Agent State. What the shell additionally needs is the **Session mark** (whose
facepile shows live Agent sessions only), and identity must be known the moment
the agent TUI starts — not at the first prompt, and not by parsing session
titles or launch commands (hand-typed `claude` is ADR 0002's core scenario). So
`SessionStart` now sends an **identity-only announce**: a report shape carrying
*which* agent with **no state field at all** — it cannot reuse `state: None`,
which means "clear identity". The announce never invents a non-null state;
exit-to-`None` clears identity along with state, reverting the session's mark to
shell.

`SessionStart` also defines a **fresh process boundary**. If it announces a
different identity or a different/new agent-native run id while the daemon still
holds the previous process's state, the daemon clears `agent_state` /
`agent_detail` to `None` and re-closes the late-arrival guard until the first
fresh `running` report. This preserves the "fresh, never-prompted agent has no
state" invariant even when a dirty agent exit is followed by an immediate
same-agent relaunch before `SessionEnd` or the foreground-command backstop clears.

The same no-confusion rule applies one layer up, on the **broadcast event**:
the announce propagates over the shared `AgentState` event carrying the
session's current — typically still-null pre-prompt — state, so a null *state*
in that event can never be the identity-clear signal (it would silently eat the
announce, deferring the Session mark to the first prompt). The event's `agent`
field is therefore optional: `agent: None` is the identity clear, `agent:
Some(..)` always (re)asserts identity, on announces and state reports alike.

Identity's *clearing* paths must mirror its announce-time exemptions:

- **Clears are exempt from the late-arrival guard.** The guard drops stale
  *state* reports (a dying agent's queued `waiting`/`error` hooks) until a
  fresh `running` arrives — but a clear is idempotent, and for a never-prompted
  agent (identity announced, guard still closed from the previous run's exit)
  the `SessionEnd` clear is the only thing that reverts the Session mark.
  Guarding it strands the mark forever.
- **The dirty-exit backstop clears on identity alone.** A never-prompted agent
  has no Agent State; the foreground-command poller must still revert its mark
  when the agent dies without `SessionEnd`.
- **Clears are idempotent** (like announces): re-clearing an already-cleared
  session — the backstop racing a late `SessionEnd` — broadcasts nothing.

**Codex notes (docs-verified 2026-06-05).** Codex CLI's hooks are real and GA
(May 2026): `<repo>/.codex/hooks.json` with the Claude-style schema is a
documented config layer, and `SessionStart`, `UserPromptSubmit`,
`PermissionRequest`, `PostToolUse`, `Stop` are all valid events. The identity
announce works for Codex only where Hitch also has a clear path: Unix installs
`SessionStart` and relies on the foreground-poller backstop; Windows skips the
Codex identity announce because Codex has no `SessionEnd` and ConPTY has no
foreground-command backstop. Three accepted gaps: (1) **no `SessionEnd` event
exists in Codex** — the installed entry is silently ignored; drop it from the
Codex overlay at implementation. Clean-exit clearing falls to the Unix backstop;
Windows avoids the mark-stranding identity announce instead. (2) **Project-local
hooks run only once the user trusts the project's `.codex` layer** (`/hooks`);
Hitch cannot auto-trust, so Codex state is silently absent until then. (3) **No
idle notification** — the `idle_prompt` heal is Claude-only. Also verify at
implementation that Codex hook subprocesses inherit the session environment
(`HITCH_SESSION_ID` is load-bearing; inheritance is undocumented).

## Windows note — no foreground-command backstop

The dirty-exit backstop above (the daemon's foreground-command poller clearing
Agent State when an agent dies without firing `SessionEnd`) is **unavailable on
Windows**. It relies on resolving the PTY's foreground process group leader, but
the Windows PTY backend is ConPTY (ADR 0012), which exposes no foreground
process group — `hitch_pty::ManagedPty::foreground_command()` returns `None`
unconditionally there. We do not implement an equivalent: ConPTY offers no
reliable, supported API for the currently-focused child, and the cases the
poller covers are already largely handled elsewhere.

Consequently, on Windows Agent State relies on (a) the agent's own hooks —
notably `SessionEnd` for a clean exit — and (b) session-exit cleanup, which
clears state to `None` when the PTY/session itself goes away (including the
Job-Object tree-kill on Session close, ADR 0012). The uncovered residue is a
dirty agent-process exit that leaves the surrounding shell session alive; on
Windows such state may linger until the next hook report or until the session
closes. Codex is narrower: because it has no `SessionEnd`, Hitch does not install
Codex's identity-only `SessionStart` announce on Windows, so a clean Codex exit
cannot strand the Codex Session mark there. This is an accepted limitation,
consistent with the Codex `error`-state gap already noted above.
