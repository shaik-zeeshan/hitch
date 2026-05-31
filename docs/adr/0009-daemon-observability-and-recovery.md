# Daemon observability and GUI-supervised recovery

The **Daemon** was opaque: it spawned detached with stdout/stderr to
`/dev/null`, so a startup crash (socket bind, store open, panic) left no trace
and the GUI only saw `wait_for_daemon` time out with "daemon did not become
ready" — no reason. The GUI tracked only its own socket link (connecting/ready/
offline) and, on a running daemon's death, dropped the user at a manual
"Reconnect" button with no auto-retry and no way to tell *why* it died or whether
a daemon was even running.

We make the daemon's health a first-class, diagnosable concern while keeping it a
GUI-supervised detached child — explicitly NOT a system service (reaffirming
[0003](0003-session-daemon.md), which rejected launchd):

- **Daemon Status** (`CONTEXT.md`) — a four-state model (*starting* / *running* /
  *unreachable* / *failed*) distinct from a single GUI's connection, surfaced in
  a persistent in-window indicator plus the tray, with the failure reason, a
  "View log" action, and a "Restart daemon" action.
- **A real log.** The detached daemon writes stdout/stderr to
  `~/.hitch/daemon.log` (beside the socket and store), rotated to
  `daemon.log.prev` on each start so a crash survives its own respawn; a panic
  hook captures worker-thread panics. The GUI reads the log tail to populate the
  *failed*/*unreachable* reason.
- **GUI-supervised recovery.** On disconnect the GUI auto-reconnects/respawns with
  backoff (showing *starting*), a `Ping`/`Pong` heartbeat makes *running* mean
  *responsive* rather than just socket-open, and a crash-loop guard stops after N
  rapid failures and shows *failed* + reason instead of thrashing.

## Considered Options

- **GUI-supervised recovery + log-tail diagnosis + four-state status** (chosen) —
  honest visibility and automatic recovery with no installed service; stays
  within ADR 0003's lightweight design. The GUI remains the supervisor, the
  daemon a detached child.
- **launchd login agent** — auto-restart on crash and survival with no GUI ever
  launched, for free. Rejected again (as in ADR 0003): installs a background
  service, needs permission, and heavier than the problem warrants. Note: this
  means the daemon does NOT restart while all windows are closed — recovery is
  driven by a GUI attaching.
- **`tracing` + `tracing-appender`** for structured, levelled logs — rejected for
  now in favor of redirecting the existing stderr to a rotating file (no new
  dependency, no instrumentation sweep); can graduate later if levels/filtering
  are needed.
- **Keep the manual Reconnect button only** — rejected; it dumps the user at a
  dead surface and answers none of "is it running / did it start / why did it
  fail."

## Consequences

- The daemon gains a log file and rotation; failure reasons are sourced from it,
  so "why did it fail?" is finally answerable.
- A new `Ping`/`Pong` protocol message backs the heartbeat (control plane only).
- The daemon still does not survive a machine reboot or run with no GUI (ADR 0003
  unchanged); recovery is triggered by a GUI attaching, with bounded auto-retry.
- Because the terminal and all git are daemon-owned (`CONTEXT.md`, ADR 0003), a
  *failed* daemon means the product surface is down by design — so the failure is
  surfaced honestly with a reason and one-click restart, rather than papered over
  with a daemon-independent fallback terminal (explicitly rejected).
