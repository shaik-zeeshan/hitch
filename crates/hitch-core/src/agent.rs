//! The [`AgentState`] reported by a known agent running in a session.

use serde::{Deserialize, Serialize};

/// The current status of a known agent running in a [`Session`](crate::Session).
///
/// Reported by the agent's own hook system over a local channel, never inferred
/// from terminal output (ADR 0002). A session not running a known agent has no
/// agent state — model that absence as `Option<AgentState>` at the call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentState {
    /// The agent is actively working.
    Running,
    /// The agent is paused, waiting for the user's approval.
    NeedsApproval,
    /// The agent finished its turn.
    Completed,
    /// The agent stopped because of an error.
    Error,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn serializes_kebab_case() {
        assert_eq!(
            serde_json::to_string(&AgentState::NeedsApproval).unwrap(),
            "\"needs-approval\""
        );
        assert_eq!(
            serde_json::to_string(&AgentState::Running).unwrap(),
            "\"running\""
        );
    }

    #[test]
    fn round_trips_through_json() {
        for state in [
            AgentState::Running,
            AgentState::NeedsApproval,
            AgentState::Completed,
            AgentState::Error,
        ] {
            let json = serde_json::to_string(&state).unwrap();
            let back: AgentState = serde_json::from_str(&json).unwrap();
            assert_eq!(state, back);
        }
    }
}
