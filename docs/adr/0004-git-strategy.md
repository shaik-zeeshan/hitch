# Focused git UI, hybrid execution, PRs via `gh`

Hitch ships a focused common-flow git UI — a changed-files list, a file-level diff viewer, stage/unstage whole files, commit, push, and create-PR — and deliberately does *not* reimplement the long tail (hunk/line staging, interactive rebase, conflict resolution, cherry-pick). For those, the user drops to the first-class terminal that is always present. Git executes as a hybrid: libgit2/`gitoxide` for fast reads (status/diff/log/branches, which get polled to show dirty indicators across many worktrees), and the system `git` CLI for writes and network ops (commit/push/fetch) so the user's hooks, config, credential helpers, and signing apply exactly as they would in the terminal. PRs go through the `gh` CLI using the user's existing `gh auth login`.

## Considered Options

- **Hybrid reads/writes** (chosen) — fast indicators without subprocess churn, faithful writes that match the terminal.
- **All system `git`** — perfect fidelity and one code path, but slow under frequent status polling.
- **All in-process (libgit2/gix)** — fastest, but may bypass the user's hooks/credential helpers/signing.
- **Full source-control client** — rejected as scope: the terminal already covers advanced git, and Hitch should not compete with it.

## Consequences

- Two git code paths (read vs write) must stay behaviourally consistent.
- PR creation requires a remote, a pushed branch, and `gh` installed + authenticated; GitHub-only initially, structured so `glab` etc. can be added later.
- Anything not in the focused UI is intentionally a terminal task, not a missing feature.
