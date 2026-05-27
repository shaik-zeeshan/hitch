# Created worktrees live in a managed global directory

Hitch places the worktrees it creates under a configurable global directory (default `~/.hitch/worktrees/<project>/<branch>/`) rather than next to the user's repository. This makes Hitch the unambiguous owner of worktree lifecycle — listing, pruning, and garbage-collecting are trivial, and the user's own folders stay clean.

## Considered Options

- **Managed global dir** (chosen) — central, easy to manage, but worktrees sit far from the original repo.
- **Sibling of the repo** (`<repo>.worktrees/<branch>`) — discoverable next to the repo, but clutters the parent directory and is harder to manage centrally.
- **Inside the repo, gitignored** — self-contained but nests working trees inside the repo, which some tooling dislikes.

## Consequences

- Anything that relies on a sibling-relative path between the worktree and the original repo will not find it; tooling must use absolute paths.
- The main worktree (the user's original checkout) is the exception — it stays wherever the user added it.
