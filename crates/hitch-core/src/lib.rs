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

/// 64-bit FNV-1a hash of `bytes`.
///
/// The single definition of the stable, non-cryptographic hash Hitch uses to
/// derive identifiers that must stay byte-for-byte reproducible across builds and
/// crates: the Windows named-pipe name (`hitch-proto`) and the managed-worktree
/// directory suffix (`hitch-git`). Both depend on this leaf crate (ADR 0005), so
/// the algorithm and its constants live here once rather than being open-coded at
/// each call site. It is intentionally dependency-free and takes raw bytes so
/// each caller keeps full control of its own input encoding.
///
/// FNV-1a (offset basis `0xcbf29ce484222325`, prime `0x00000100000001b3`): seed
/// with the basis, then for each byte XOR it in and multiply by the prime. The
/// output is stability-sensitive — do not change the algorithm or constants.
pub fn fnv1a_64(bytes: &[u8]) -> u64 {
    const FNV_OFFSET_BASIS: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x00000100000001b3;

    let mut hash = FNV_OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(FNV_PRIME);
    }
    hash
}

#[cfg(test)]
mod fnv_tests {
    use super::fnv1a_64;

    #[test]
    fn fnv1a_64_matches_known_vectors() {
        // Canonical FNV-1a/64 test vectors (offset basis for the empty input,
        // and the well-known "a"/"foobar" reference values). These pin the
        // algorithm and constants so the pipe names and worktree directory
        // suffixes derived from this hash can never silently shift.
        assert_eq!(fnv1a_64(b""), 0xcbf29ce484222325);
        assert_eq!(fnv1a_64(b"a"), 0xaf63dc4c8601ec8c);
        assert_eq!(fnv1a_64(b"foobar"), 0x85944171f73967e8);
    }
}
