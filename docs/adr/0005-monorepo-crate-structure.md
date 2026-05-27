# Monorepo Cargo workspace: fine-grained crates, daemon as sole composer

Hitch is a single monorepo with a Cargo workspace of fine-grained, single-purpose crates plus the Tauri app. The dependency graph is a strict DAG enforcing crate independence: shared *leaf* crates at the bottom, *feature* crates that depend only on the leaves and **never on each other**, and the daemon as the single composition point.

## Layout

```
crates/
  hitch-core     domain types (Project, Worktree, Session, AgentState)   -> (leaf)
  hitch-proto    IPC message enum + framing                              -> core
  hitch-git      reads via gix/libgit2, writes via system git, PRs via gh -> core
  hitch-pty      PTY spawn + scrollback buffering                        -> core
  hitch-store    SQLite persistence                                      -> core
  hitch-agent    agent registry + hook install + state parsing           -> core
  hitch-daemon   BIN: composes git+pty+store+agent, serves the socket     -> all feature crates + proto
  hitch-hook     BIN: tiny hook helper, reports agent state               -> proto
apps/desktop/
  src/           Svelte/TypeScript frontend (Vite)                        -> see 0006
  src-tauri/     BIN: thin Tauri client                                   -> proto (+ core)
```

## Rules

- **Feature crates (`hitch-git`, `hitch-pty`, `hitch-store`, `hitch-agent`) depend only on `hitch-core` (+ external deps), never on each other.** This is what makes them independently testable and replaceable.
- **The daemon is the only crate that links feature crates together.** All composition lives there.
- **`src-tauri` is a thin IPC client** — it depends only on `hitch-proto` (+`hitch-core`) and routes every operation (git reads and writes, sessions, agent state) through the daemon over the socket. No feature-crate logic in the GUI process; the daemon is the single source of truth.
- **`hitch-proto` owns the wire contract** shared by daemon, `src-tauri`, and `hitch-hook`: a Unix domain socket carrying a serde-tagged message enum — JSON-framed for the control plane (debuggable via `nc`) and length-prefixed raw bytes for PTY data streams.

## Considered Options

- **Fine-grained crates** (chosen) over coarse grouping (e.g. bundling pty+store as one "session" crate) — more seams, but maximum isolation, which matches the stated goal.
- **GUI links read-only crates** (e.g. `hitch-git` in `src-tauri` for fast diffs) — rejected: puts git logic in two processes, risking races and duplicated rules. The local-socket round-trip is cheap and the daemon is mandatory anyway.
- **Fully binary IPC (postcard)** and **JSON-RPC everywhere (base64 PTY)** — rejected in favour of JSON control + raw PTY frames: debuggable where it helps, efficient where it matters.

## Consequences

- The Svelte frontend (see [0006](0006-frontend-stack.md)) talks to `src-tauri` via Tauri's own IPC; only Rust processes speak the daemon socket, so the proto encoding is a Rust-to-Rust concern.
- A single `package.json` under `apps/desktop` suffices for now (one frontend app); no JS package-manager workspace is introduced until there is more than one TS package.
- See [0003](0003-session-daemon.md) for why the daemon exists and [0002](0002-agent-state-via-hooks.md) for what `hitch-agent`/`hitch-hook` carry over the socket.
