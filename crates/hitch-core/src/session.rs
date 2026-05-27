//! The [`Session`] entity: a single PTY process — the unit of work in Hitch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, SessionId, WorktreeId};

/// Where a [`Session`] lives.
///
/// A session runs in exactly one git [`Worktree`](crate::Worktree), or — for a
/// plain-folder project — directly in the project root (see CONTEXT.md).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "kind", content = "id")]
pub enum SessionParent {
    /// A session in a git-backed project's worktree.
    Worktree(WorktreeId),
    /// A session in a plain-folder project's root.
    Project(ProjectId),
}

/// A single PTY process running in a fixed working directory.
///
/// Sessions are owned by the daemon (ADR 0003), nameable, and there may be
/// several per worktree. There is no separate "task" entity — a session running
/// an agent surfaces an [`AgentState`](crate::AgentState); that is the closest thing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Session {
    pub id: SessionId,
    /// User-facing name for the session.
    pub name: String,
    /// The worktree or plain-folder project this session belongs to.
    pub parent: SessionParent,
    /// Working directory the PTY is spawned in.
    pub cwd: PathBuf,
}

impl Session {
    /// Create a session with a freshly generated [`SessionId`].
    pub fn new(name: impl Into<String>, parent: SessionParent, cwd: impl Into<PathBuf>) -> Self {
        Self {
            id: SessionId::new(),
            name: name.into(),
            parent,
            cwd: cwd.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parent_is_adjacently_tagged() {
        let parent = SessionParent::Worktree(WorktreeId::from(uuid_nil()));
        let json = serde_json::to_string(&parent).unwrap();
        assert_eq!(
            json,
            r#"{"kind":"worktree","id":"00000000-0000-0000-0000-000000000000"}"#
        );
    }

    #[test]
    fn round_trips_both_parents() {
        for parent in [
            SessionParent::Worktree(WorktreeId::new()),
            SessionParent::Project(ProjectId::new()),
        ] {
            let session = Session::new("shell", parent, "/tmp");
            let json = serde_json::to_string(&session).unwrap();
            let back: Session = serde_json::from_str(&json).unwrap();
            assert_eq!(session, back);
        }
    }

    fn uuid_nil() -> uuid::Uuid {
        uuid::Uuid::nil()
    }
}
