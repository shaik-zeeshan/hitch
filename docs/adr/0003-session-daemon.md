# Sessions are owned by a long-lived detached daemon, not the GUI

To keep terminal sessions (and their running processes, including long agent runs) alive across an app quit/reopen, Hitch runs a separate long-lived daemon — a sidecar Rust binary that owns every PTY, buffers scrollback, and receives agent hook notifications. The GUI is a thin client that connects to the daemon over a local socket and reattaches to live sessions on launch. The GUI spawns the daemon detached on first run; it survives window-close and Cmd-Q, signalled by a menu-bar item, and stops only on an explicit "Quit Hitch completely."

## Considered Options

- **GUI-spawned detached daemon + menu-bar** (chosen) — lightest design that keeps sessions alive across app restart; no installed system service.
- **launchd login agent** — always-on, auto-restart on crash, fully GUI-independent; heavier (installs a background service, needs permission).
- **No daemon (processes are GUI children)** — simplest, but sessions die on quit; rejected because persistence across restart was a hard requirement.

## Consequences

- PTYs must not be children of the GUI process; all PTY/scrollback/agent-hook plumbing lives in the daemon, and the GUI talks to it over IPC.
- Hitch has a background presence after Cmd-Q; a menu-bar item is required to make that honest and to offer full quit.
- A machine reboot still loses live processes; across a reboot Hitch falls back to reopening the saved layout as fresh terminals (see [0001](0001-managed-worktree-location.md) for where worktrees live).
- Agent hooks report into the daemon (always alive), and the GUI subscribes to state changes.
