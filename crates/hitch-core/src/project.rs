//! The [`Project`] entity: a directory the user added to Hitch.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::ids::ProjectId;

/// Whether a [`Project`] is rooted at a git repository or a plain folder.
///
/// Git-backed projects support worktrees and git operations; plain folders
/// expose terminals only (see CONTEXT.md and ADR 0004).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProjectKind {
    /// Rooted at a git repository.
    GitBacked,
    /// A plain folder, with no git operations.
    Plain,
}

/// A workspace rooted at a single local directory the user added to Hitch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Project {
    pub id: ProjectId,
    /// Display name (by convention the root directory's file name).
    pub name: String,
    /// Absolute path to the directory the user added.
    pub root: PathBuf,
    pub kind: ProjectKind,
}

impl Project {
    /// Create a project with a freshly generated [`ProjectId`].
    pub fn new(name: impl Into<String>, root: impl Into<PathBuf>, kind: ProjectKind) -> Self {
        Self {
            id: ProjectId::new(),
            name: name.into(),
            root: root.into(),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kind_serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&ProjectKind::GitBacked).unwrap(),
            "\"git-backed\""
        );
        assert_eq!(
            serde_json::to_string(&ProjectKind::Plain).unwrap(),
            "\"plain\""
        );
    }

    #[test]
    fn round_trips_through_json() {
        let project = Project::new("hitch", "/Users/me/Code/hitch", ProjectKind::GitBacked);
        let json = serde_json::to_string(&project).unwrap();
        let back: Project = serde_json::from_str(&json).unwrap();
        assert_eq!(project, back);
    }
}
