//! Strongly-typed, opaque identifiers for the domain entities.
//!
//! Each id is a newtype around a random [`Uuid`]. Distinct types stop a
//! `WorktreeId` being passed where a `SessionId` is expected, and `transparent`
//! serde means the wire/storage form is just the bare UUID string.

use std::fmt;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! id_type {
    ($(#[$meta:meta])* $name:ident) => {
        $(#[$meta])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
        #[serde(transparent)]
        pub struct $name(Uuid);

        impl $name {
            /// Generate a fresh, random id.
            pub fn new() -> Self {
                Self(Uuid::new_v4())
            }

            /// The underlying UUID.
            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl From<Uuid> for $name {
            fn from(id: Uuid) -> Self {
                Self(id)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                fmt::Display::fmt(&self.0, f)
            }
        }
    };
}

id_type! {
    /// Identifies a [`crate::Project`].
    ProjectId
}

id_type! {
    /// Identifies a [`crate::Worktree`].
    WorktreeId
}

id_type! {
    /// Identifies a [`crate::Session`].
    SessionId
}

id_type! {
    /// Identifies a long-running daemon **Job** (see `CONTEXT.md`). Jobs are
    /// ephemeral and never persisted, but they still get an opaque random id so
    /// the GUI can correlate `JobStarted` / `JobProgress` / `JobCompleted`.
    JobId
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_ids_are_unique() {
        assert_ne!(ProjectId::new(), ProjectId::new());
    }

    #[test]
    fn serializes_as_a_bare_uuid_string() {
        let id = WorktreeId::new();
        let json = serde_json::to_string(&id).unwrap();
        assert_eq!(json, format!("\"{}\"", id.as_uuid()));
    }

    #[test]
    fn round_trips_through_json() {
        let id = SessionId::new();
        let json = serde_json::to_string(&id).unwrap();
        let back: SessionId = serde_json::from_str(&json).unwrap();
        assert_eq!(id, back);
    }

    #[test]
    fn display_matches_underlying_uuid() {
        let id = ProjectId::new();
        assert_eq!(id.to_string(), id.as_uuid().to_string());
    }
}
