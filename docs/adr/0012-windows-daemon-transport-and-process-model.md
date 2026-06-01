# Windows Daemon transport is a named pipe; process control is Job Objects

Hitch's **Daemon** path is built on Unix primitives that do not exist on Windows:
a Unix-domain socket in `$TMPDIR` (`hitch-<uid>.sock`), a sibling pidfile held
under an `flock` advisory lock for force-kill of an un-handshakeable daemon
(ADR [0009](0009-daemon-observability-and-recovery.md)), `kill`/`SIGKILL` by pid,
PTY process groups (`setsid` + `kill(-pgid, …)`, ADR
[0010](0010-daemon-forces-child-repaint.md)), and a `~/.hitch` data root. Before
porting the Daemon to a native Windows target (parent #9) we fix the
Windows-native transport and process-lifecycle model so the rest of the port has
a stable contract to build against.

The framing layer (ADR [0005](0005-monorepo-crate-structure.md)) is already
transport-neutral — newline-delimited JSON for the control plane and
length-prefixed raw bytes for PTY data are byte streams over *any* reliable,
ordered, local pipe — so only the connection primitive and the
discovery/recovery/process-kill mechanics around it need a Windows answer. We
choose **Windows named pipes** for transport and **Win32 Job Objects** for
process-tree control, keeping ADR 0003/0009's "GUI-supervised detached child, not
a system service" model intact.

## Decision

### Transport — a per-user named pipe

- The Daemon serves a named pipe `\\.\pipe\hitch<-dev>-<user>` (the
  `<-dev>` infix mirrors `instance_namespace()`, so a debug build and an installed
  release build stay fully isolated, exactly as on Unix). The **GUI** and the
  **hook helper** (`hitch-hook`) both dial this one name; control and PTY frames
  multiplex over it unchanged.
- The pipe is created with an SDDL security descriptor that grants access to the
  owning user only — the named-pipe equivalent of a `0700` socket — so other
  local users cannot attach to another user's Sessions.
- `hitch-proto::transport` gains a `#[cfg(windows)]` connection type beside the
  existing `#[cfg(unix)]` `UnixSocket*`. To keep one connection-lifecycle path
  (reader thread + heartbeat + cloned writer half, as today), we adopt the
  `interprocess` crate's blocking local-socket abstraction (Unix socket on Unix,
  named pipe on Windows); the framing/decoders are untouched.

### Discovery, restart, and termination — no socket file, no pidfile lock, no signals

- **Discovery is the pipe name itself.** It is derived deterministically from
  user + namespace, so there is no `$TMPDIR` socket file and no rendezvous/port
  file to find, stale-check, or clean up. A dead daemon leaves *no* artifact: the
  kernel reclaims the pipe's server instances when the process exits, so
  connecting either succeeds (daemon alive) or fails with `FILE_NOT_FOUND`
  (no daemon). The Unix `remove_stale_socket` path and its stale-socket race
  simply do not exist on Windows.
- **Liveness** drops the `flock`-on-pidfile probe. To force-kill an
  un-handshakeable daemon (an upgrade/version mismatch that never returns a pid in
  `Hello`), the client calls `GetNamedPipeServerProcessId` on its connected pipe
  handle — the OS reports the *server's* pid directly, with no pid-reuse hazard
  and no file to read or lock. This replaces the pidfile entirely.
- **Termination** uses `OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess`
  in place of `kill(pid, SIGKILL)`. Graceful shutdown is unchanged: the existing
  `ShutdownDaemon` control message travels over the pipe, so no `SIGTERM` analog
  is needed.
- **Restart** keeps ADR 0009's shape — terminate, wait for the pipe name to stop
  accepting connections, then re-spawn the detached daemon — with
  `wait_for_socket_release` reimplemented as a pipe-availability poll. The
  four-state **Daemon Status** model, crash-loop guard, and `Ping`/`Pong`
  heartbeat are preserved verbatim; only the liveness primitive changes.

### Process-tree cancellation — Win32 Job Objects

- Each Session's PTY child and each cancellable **Job**'s git/agent child
  (ADR [0008](0008-async-jobs-off-request-loop.md)) is assigned to its own Win32
  Job Object created with `JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE`. This is the
  Windows analog of the Unix process group (`setsid` + `kill(-pgid)`).
- Killing a Session, cancelling a Job, or shutting the Daemon down calls
  `TerminateJobObject`, which kills the child **and every descendant atomically**.
  Agents fan out into subprocesses (e.g. `node`/`python` helpers); terminating the
  tracked leaf alone would orphan them, which is exactly the failure Job Objects
  prevent.
- The PTY backend on Windows is ConPTY, already available through the
  `portable-pty` dependency. The Unix forced-repaint hack
  (`kill(-pgid, SIGWINCH)`, ADR 0010) stays `#[cfg(unix)]`: ConPTY reflows and
  repaints natively on `ResizePseudoConsole`, so Windows needs no equivalent.

### Data locations — `%LOCALAPPDATA%\Hitch`

- The Windows data root is `%LOCALAPPDATA%\Hitch` (`Hitch-dev` for the debug
  namespace) via `dirs::data_local_dir()` — **Local**, not Roaming, because the
  store and managed worktrees are large and machine-local; roaming them would sync
  git checkouts across machines. This replaces `~/.hitch`.
  - **Store** (SQLite): `%LOCALAPPDATA%\Hitch\store.sqlite`.
  - **Managed Worktrees**: `%LOCALAPPDATA%\Hitch\worktrees\<project>\<branch>\`,
    honoring ADR [0001](0001-managed-worktree-location.md)'s managed-global-dir
    decision with a Windows-appropriate root.
  - **Logs**: `%LOCALAPPDATA%\Hitch\logs\daemon.log` (+ `daemon.log.prev`),
    keeping ADR 0009's rotate-on-start scheme.
  - **Per-instance state**: the namespace (`dev`/release) is applied to both the
    pipe name and the data-dir suffix, so builds never share a daemon, store, or
    worktree set. The Unix socket/pidfile-in-`$TMPDIR` concepts retire — there is
    no filesystem rendezvous on Windows.
- **MAX_PATH (260) is a real risk** for deep worktrees under a long
  `%LOCALAPPDATA%` root. Mitigation: prefix managed-worktree filesystem access
  with the extended-length `\\?\` form and ship a long-path-aware app manifest;
  surface a clear error rather than a truncated path if a checkout exceeds the
  limit on a system without long paths enabled.

## Considered Options

- **Named pipes** (chosen) — the native Windows analog of a Unix socket: a stable
  well-known name (no port/rendezvous file), per-user ACLs via SDDL, server-pid
  discovery via `GetNamedPipeServerProcessId` (retires the pidfile + advisory
  lock), and no stale filesystem artifact on death. Reuses the transport-neutral
  framing with only a new connection type.
- **localhost TCP** — rejected: an ephemeral port reintroduces the
  discovery-file problem we just eliminated, the port is reachable by any local
  process/user without an added auth token, it can trip Windows Firewall prompts,
  and it carries no built-in peer identity. Named pipes give security and a stable
  name for free.
- **AF_UNIX on Windows** (Win10+ supports `SOCK_STREAM` AF_UNIX) — tempting for
  code parity, but it brings back the stale socket *file* (the exact thing we shed
  with pipes), has no Rust std support, and offers no upside over a pipe.
  Rejected.
- **`TerminateProcess` on the tracked child only** — rejected for process kill:
  it orphans grandchildren (agent subprocesses outlive the cancel), the precise
  defect Job Objects exist to prevent.
- **Console control events** (`CREATE_NEW_PROCESS_GROUP` + `CTRL_BREAK`) —
  rejected: cooperative-only, unreliable as a hard kill, doesn't guarantee
  grandchild death, and ConPTY already owns the console. Job Objects are the
  deterministic tree-kill.

## Consequences

- `hitch-proto::transport` becomes platform-split: the `#[cfg(unix)]`
  `os::unix::net` types are joined by a `#[cfg(windows)]` named-pipe type behind
  the `interprocess` local-socket abstraction; framing (ADR 0005) and the message
  enum are untouched, so the daemon, `src-tauri`, and `hitch-hook` keep one wire
  contract.
- The pidfile, its `flock` advisory lock, and `remove_stale_socket` are Unix-only;
  the Windows recovery path uses `GetNamedPipeServerProcessId` + `TerminateProcess`
  while preserving ADR 0009's four-state **Daemon Status**, crash-loop guard, and
  heartbeat.
- `hitch-pty` grows a `#[cfg(windows)]` Job-Object wrapper around spawned
  children; cancellable **Jobs** (ADR 0008) and Session shutdown cancel via
  `TerminateJobObject` instead of signalling a process group. The
  forced-repaint path (ADR 0010) stays Unix-only.
- Data moves to `%LOCALAPPDATA%\Hitch` on Windows (vs `~/.hitch` on Unix), with a
  documented `\\?\` long-path mitigation for managed worktrees (ADR 0001).
- The **hook helper** dials the same per-user pipe name; its `HITCH_SESSION_ID`
  resolution (CONTEXT.md) is transport-independent and unchanged.
- "Detached GUI-supervised child, not a system service," no survival across
  reboot, and recovery driven by a GUI attaching (ADR 0003/0009) all hold on
  Windows — only the underlying primitives differ.
