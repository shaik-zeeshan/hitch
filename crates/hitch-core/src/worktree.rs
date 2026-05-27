//! The [`Worktree`] entity: a git working tree belonging to a git-backed project.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, WorktreeId};

/// A git working tree belonging to a git-backed [`crate::Project`], checked out
/// on exactly one branch.
///
/// The original directory the user added is the *main* worktree (`is_main`);
/// every other worktree is one Hitch created under its managed directory
/// (ADR 0001). Git enforces that a branch is checked out in at most one worktree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: WorktreeId,
    /// The git-backed project this worktree belongs to.
    pub project_id: ProjectId,
    /// Absolute path to the working tree on disk.
    pub path: PathBuf,
    /// The branch checked out in this worktree.
    pub branch: String,
    /// True for the original directory the user added (the main worktree).
    pub is_main: bool,
}

impl Worktree {
    /// Create a worktree with a freshly generated [`WorktreeId`].
    pub fn new(
        project_id: ProjectId,
        path: impl Into<PathBuf>,
        branch: impl Into<String>,
        is_main: bool,
    ) -> Self {
        Self {
            id: WorktreeId::new(),
            project_id,
            path: path.into(),
            branch: branch.into(),
            is_main,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_through_json() {
        let worktree = Worktree::new(ProjectId::new(), "/Users/me/.hitch/wt/x", "feat/x", false);
        let json = serde_json::to_string(&worktree).unwrap();
        let back: Worktree = serde_json::from_str(&json).unwrap();
        assert_eq!(worktree, back);
    }
}
