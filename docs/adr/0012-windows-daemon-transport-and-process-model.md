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

### Transport — a per-user local socket endpoint

- The Daemon and its clients use `hitch-proto::transport::default_socket_path()` as
  the stable logical endpoint. On Windows this is a path under the per-instance
  Local AppData root (for example `%LOCALAPPDATA%\Hitch\daemon.sock`), but no
  socket file is created there: the transport hashes that logical path and asks
  `interprocess` for a generic namespaced local socket, which is backed by a
  Windows named pipe. The **GUI** and the **hook helper** (`hitch-hook`) both pass
  the same logical path to the transport; control and PTY frames multiplex over it
  unchanged.
- The named-pipe endpoint created by the transport is per-user — the named-pipe
  equivalent of a `0700` socket — so other local users cannot attach to another
  user's Sessions. This is not the default DACL: the transport binds the listener
  through `interprocess`' `ListenerOptionsExt::security_descriptor` with an
  owner-only descriptor deserialized from the SDDL `D:P(A;;GA;;;OW)` — a protected
  DACL granting `GenericAll` to the creator-owner SID and, because a non-null DACL
  denies every unlisted principal, nothing to anyone else. The creating user is
  the daemon's user, and all clients (GUI, hook) run as that same user, so
  owner-only is both the tightest and the sufficient grant; SYSTEM/Administrators
  ACEs are deliberately omitted.
- `hitch-proto::transport` gains a `#[cfg(windows)]` connection type beside the
  existing `#[cfg(unix)]` `UnixSocket*`. To keep one connection-lifecycle path
  (reader thread + heartbeat + cloned writer half, as today), we adopt the
  `interprocess` crate's blocking local-socket abstraction (Unix socket on Unix,
  named pipe on Windows); the framing/decoders are untouched.

#### Accept model — a blocking accept thread, not a nonblocking poll

The listener stays in **blocking** mode. The daemon dedicates one thread
(`hitch-accept`) that parks in `accept()` and forwards each accepted stream over
an mpsc channel to the main thread, which switches it to blocking and spawns the
per-client handler as before. This replaces an earlier nonblocking-poll loop
(nonblocking listener + 25 ms sleep, re-checking the shutdown flag each tick).

The poll loop had a real defect on Windows named pipes: between two polls there
is no armed pipe instance, so a client that connected, wrote, and closed inside
that window was never accepted and its bytes were silently dropped — fatal for
fire-and-forget hook reports. A parked blocking `accept()` always has an armed
instance, eliminating that dominant gap.

- **Shutdown wake.** A blocking `accept()` cannot see the shutdown flag while
  parked. The `ShutdownDaemon` handler therefore sets the flag and then makes a
  best-effort connect to the daemon's own endpoint (immediately dropped). That
  connect completes the pending `ConnectNamedPipe`, unblocking the accept thread,
  which re-reads the flag, sees it set, and exits without arming another accept.
  The connect retries briefly past a transient `ERROR_PIPE_BUSY` (the thread
  re-arms a fresh instance between connections) and gives up on NotFound/refused
  (the listener is already gone). The main loop joins the accept thread on the
  way out.
- **Residual `ERROR_PIPE_BUSY` re-arm gap.** A *narrow* window remains between an
  `accept()` returning and the next `accept()` being issued, during which a
  concurrent connect can still observe `ERROR_PIPE_BUSY` (231). This is far
  smaller than the old poll gap (microseconds of re-arm vs. up to 25 ms of sleep)
  but it is not zero, so it is a transient "all instances busy" state on a *live*
  server, not an absent one.
- **Why client ack-wait and busy-retry stay.** Two reasons, both deliberate:
  1. *Re-arm gap.* Because the busy gap above survives, every client keeps the
     `ERROR_PIPE_BUSY` retry (`is_endpoint_busy`, `endpoint_accepts_connections`):
     a busy endpoint means a live daemon to reconnect to, not a dead one.
  2. *Stale-daemon compatibility.* A newer hook can talk to an **older** daemon
     that still runs the nonblocking poll loop — Hitch bundles daemon sidecars by
     copy and `Hello` only checks the protocol version, so a stale daemon binary
     can be the one actually serving. The hook therefore keeps holding the pipe
     open and waiting for the daemon's ack (with its watchdog) regardless of which
     accept model the current source uses; that ack-wait is what made delivery
     survive a polling daemon in the first place and must not be removed on the
     strength of this change.

### Discovery, restart, and termination — no socket file, no pidfile lock, no signals

- **Discovery is the logical socket path itself.** It is derived deterministically
  from the per-instance Local AppData root, then translated by
  `hitch-proto::transport` into the actual local-socket/named-pipe endpoint. There
  is no `$TMPDIR` socket file and no rendezvous/port file to find, stale-check, or
  clean up. A dead daemon leaves no client-visible filesystem socket artifact: the
  kernel reclaims the pipe's server instances when the process exits, so connecting
  either succeeds (daemon alive) or fails (no daemon). The Unix
  `remove_stale_socket` path and its stale-socket race simply do not exist on
  Windows.
- **Liveness** drops the `flock`-on-pidfile probe. After a successful `Hello`,
  the client caches the daemon pid returned by the protocol and uses that pid for
  non-cooperative termination. For an incompatible daemon that responds but does
  not complete `Hello`, the client may call `GetNamedPipeServerProcessId` only
  after a response has been received; calling it during attach can block before
  the server accepts the pipe and leave the GUI stuck in *starting*. In the
  implementation the client obtains this server pid through `interprocess`'
  `peer_creds().pid()` on the connected client-side stream, which is the safe
  wrapper over `GetNamedPipeServerProcessId` — the same primitive named here, by
  another name.
- **Termination** uses `OpenProcess(PROCESS_TERMINATE)` + `TerminateProcess`
  in place of `kill(pid, SIGKILL)`. Graceful shutdown is unchanged: the existing
  `ShutdownDaemon` control message travels over the pipe, so no `SIGTERM` analog
  is needed.
- **Restart** keeps ADR 0009's shape — terminate, wait for the logical endpoint to
  stop accepting connections, then re-spawn the detached daemon — with
  `wait_for_socket_release` reimplemented as an endpoint-availability poll. The
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

- The Windows data root is `%LOCALAPPDATA%\Hitch` via the transport-owned
  default data-dir helper — **Local**, not Roaming, because the store and managed
  worktrees are large and machine-local; roaming them would sync git checkouts
  across machines. Release uses the root directly; debug/custom namespaces use a
  child directory such as `%LOCALAPPDATA%\Hitch\dev`. This replaces `~/.hitch`.
  - **Store** (SQLite): `<data-root>\hitch.sqlite`.
  - **Managed Worktrees**: `<data-root>\worktrees\<project>\<branch>\`, honoring
    ADR [0001](0001-managed-worktree-location.md)'s managed-global-dir decision
    with a Windows-appropriate root.
  - **Logs**: `<data-root>\daemon.log` (+ `daemon.log.prev`), keeping ADR 0009's
    rotate-on-start scheme.
  - **Per-instance state**: the namespace (`dev`/release) is applied to the
    logical socket path and the data directory, so builds never share a daemon,
    store, or worktree set. The Unix socket/pidfile-in-`$TMPDIR` concepts retire —
    there is no filesystem rendezvous on Windows.
- **MAX_PATH (260) is a real risk** for deep worktrees under a long
  `%LOCALAPPDATA%` root. Mitigation: prefix managed-worktree filesystem access
  with the extended-length `\\?\` form and ship a long-path-aware app manifest;
  surface a clear error rather than a truncated path if a checkout exceeds the
  limit on a system without long paths enabled.

## Considered Options

- **Named pipes through `interprocess` local sockets** (chosen) — the native
  Windows analog of a Unix socket: clients share a stable logical path under Local
  AppData, while the transport translates that path into the actual per-user
  namespaced endpoint. This avoids a port/rendezvous file, keeps server-pid
  discovery via `GetNamedPipeServerProcessId` (retiring the pidfile + advisory
  lock), and leaves no stale filesystem artifact on death. Reuses the
  transport-neutral framing with only a new connection type.
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
  `os::unix::net` types are joined by a `#[cfg(windows)]` local-socket type behind
  the `interprocess` abstraction. Windows callers pass the same logical socket
  path as the daemon; the transport owns the hash/translation to the named-pipe
  endpoint. Framing (ADR 0005) and the message enum are untouched, so the daemon,
  `src-tauri`, and `hitch-hook` keep one wire contract.
- The pidfile, its `flock` advisory lock, and `remove_stale_socket` are Unix-only;
  the Windows recovery path uses `GetNamedPipeServerProcessId` + `TerminateProcess`
  while preserving ADR 0009's four-state **Daemon Status**, crash-loop guard, and
  heartbeat.
- A `#[cfg(windows)]` Job-Object wrapper wraps spawned children; cancellable
  **Jobs** (ADR 0008) and Session shutdown cancel via `TerminateJobObject`
  instead of signalling a process group. The forced-repaint path (ADR 0010)
  stays Unix-only. *(Amended 2026-06-04: the wrapper was originally placed in
  `hitch-pty`, but `hitch-git`'s cancellable git commands need it too, so it
  lives in the leaf crate `hitch-process` — see ADR 0005's amendment.)*
- Data moves to `%LOCALAPPDATA%\Hitch` on Windows (vs `~/.hitch` on Unix), with
  namespaced debug/custom children and a documented `\\?\` long-path mitigation
  for managed worktrees (ADR 0001).
- The **hook helper** dials the same logical socket path; its `HITCH_SESSION_ID`
  resolution (CONTEXT.md) is transport-independent and unchanged.
- "Detached GUI-supervised child, not a system service," no survival across
  reboot, and recovery driven by a GUI attaching (ADR 0003/0009) all hold on
  Windows — only the underlying primitives differ.
