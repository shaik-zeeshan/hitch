//! `hitch-proto` — the Hitch IPC wire contract (ADR 0005).
//!
//! This crate is the shared contract spoken by `hitch-daemon`, `src-tauri`, and
//! `hitch-hook`. It deliberately depends only on `hitch-core` plus serialization
//! crates so the daemon remains the sole composer of feature crates.
//!
//! # Wire shape
//!
//! - Control plane: newline-delimited JSON [`ControlMessage`] values.
//! - PTY data plane: raw bytes framed as a four-byte big-endian length followed
//!   by that many payload bytes. The adjacent control message carries session
//!   context (`SessionOutput` / `SendSessionInput`) so PTY payloads stay out of
//!   JSON.
//!
//! # Message catalog
//!
//! [`ControlMessage`] is a serde-tagged enum with three families:
//!
//! - [`Request`]: client/hook commands such as project/worktree/session/git ops,
//!   daemon shutdown, and agent hook state reports.
//! - [`Response`]: command results or structured [`ProtocolError`] values.
//! - [`Event`]: server-push updates: session output availability, agent state,
//!   worktree dirty changes, and entity lifecycle notifications.
//!
//! `MESSAGE_CATALOG` contains the same catalog as a runtime string for generated
//! diagnostics or future docs tooling.

pub mod framing;
pub mod message;
#[cfg(unix)]
pub mod transport;

pub use framing::{
    decode_pty_frame, encode_control_message, encode_pty_frame, ControlLineDecoder, FrameError,
    PtyFrameDecoder, MAX_PTY_FRAME_LEN,
};
pub use message::*;

/// Human-readable catalog of the stable control-plane message families.
pub const MESSAGE_CATALOG: &str = r#"
ControlMessage:
  request(id, request): Request
  response(id, response): Response
  event(event): Event

Request:
  hello(client_name, protocol_version)
  shutdown-daemon
  list-projects
  add-project(root)
  clone-project(remote_url, destination, name?)
  remove-project(project_id, force)
  list-worktrees(project_id)
  create-worktree(project_id, branch, base?, mode)
  remove-worktree(worktree_id, delete_branch, force)
  list-sessions(parent?)
  open-session(parent, name, command?)
  close-session(session_id, kill_process)
  rename-session(session_id, name)
  send-session-input(session_id, byte_count)
  resize-session(session_id, cols, rows)
  git-status(worktree_id)
  git-diff(worktree_id, path)
  stage-files(worktree_id, paths)
  unstage-files(worktree_id, paths)
  discard-files(worktree_id, paths)
  commit(worktree_id, subject, body?)
  list-draft-models(provider)
  generate-commit-draft(worktree_id, settings?)
  generate-pull-request-draft(worktree_id, base?, settings?)
  push(worktree_id)
  create-pull-request(worktree_id, title, body?, base?, draft)
  install-agent-hooks(worktree_id)
  report-agent-state(agent, state, session_id?, cwd?, detail?)

Response:
  hello(protocol_version)
  ack
  projects(projects)
  worktrees(worktrees)
  sessions(sessions)
  session-opened(session)
  git-status(status)
  file-diff(diff)
  pull-request-created(url)
  commit-draft(draft)
  pull-request-draft(draft)
  error(error)

Event:
  session-opened(session)
  session-closed(session_id, exit_code?)
  session-output(session_id, byte_count)
  agent-state(session_id?, worktree_id?, agent, state, detail?)
  worktree-dirty(worktree_id, dirty)
  worktree-updated(worktree)
  project-updated(project)
"#;
