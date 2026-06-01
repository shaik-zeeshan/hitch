//! `hitch-core` — shared domain types for Hitch.
//!
//! The leaf of the workspace DAG (ADR 0005): this crate defines the vocabulary
//! every other crate speaks — [`Project`], [`Worktree`], [`Session`],
//! [`AgentState`], and their ids — and depends on nothing else in the workspace.
//! Every type is `serde`-serializable so it can cross the `hitch-proto` wire and
//! land in the `hitch-store` SQLite database unchanged.

mod agent;
mod ids;
mod project;
mod session;
mod worktree;

pub use agent::AgentState;
pub use ids::{JobId, ProjectId, SessionId, WorktreeId};
pub use project::{Project, ProjectKind};
pub use session::{Session, SessionParent};
pub use worktree::Worktree;

/// Environment variable Hitch sets in every PTY session so agent hooks launched
/// from that shell can report state against the correct Hitch session tab.
pub const SESSION_ID_ENV: &str = "HITCH_SESSION_ID";
