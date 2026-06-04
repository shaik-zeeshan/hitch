# Created worktrees live in a managed global directory

Hitch places the worktrees it creates under a configurable global directory (default `~/.hitch/worktrees/<project>/<branch>/`) rather than next to the user's repository. This makes Hitch the unambiguous owner of worktree lifecycle — listing, pruning, and garbage-collecting are trivial, and the user's own folders stay clean.

## Considered Options

- **Managed global dir** (chosen) — central, easy to manage, but worktrees sit far from the original repo.
- **Sibling of the repo** (`<repo>.worktrees/<branch>`) — discoverable next to the repo, but clutters the parent directory and is harder to manage centrally.
- **Inside the repo, gitignored** — self-contained but nests working trees inside the repo, which some tooling dislikes.

## Consequences

- Anything that relies on a sibling-relative path between the worktree and the original repo will not find it; tooling must use absolute paths.
- The main worktree (the user's original checkout) is the exception — it stays wherever the user added it.

## Amendment (2026-06-04): per-component sanitization, stable hash, and length cap

The original `<project>/<branch>/` layout used branch and project names verbatim as path components. That breaks on Windows — a branch like `feature/a:b*?` contains characters (`/ \ : * ? " < > |`, control chars) that are illegal in a path component, names that trim to empty or to a reserved device name (`CON`, `NUL`, `COM1`, …) are invalid, and long branch names eat into the MAX_PATH (260) budget that ADR [0012](0012-windows-daemon-transport-and-process-model.md) already fights for under a long `%LOCALAPPDATA%\Hitch` root.

So each component is now derived by `hitch-git`'s `safe_path_component` rather than used raw, on **all** platforms (one cross-platform scheme keeps a project's worktree path identical everywhere, so the store and the GUI agree regardless of OS):

- Windows-invalid characters and control characters are replaced with `_`; leading/trailing dots and spaces are trimmed; an empty result becomes `_`; a Windows reserved device name is prefixed with `_`.
- The sanitized prefix is truncated to 96 characters, leaving headroom under MAX_PATH for the managed root and the `.git`/nested paths a checkout adds.
- A stable 64-bit FNV-1a hash of the **original** name is appended as `-<hex>`. Sanitization and truncation are lossy and many-to-one (`feature/a` and `feature\a` collapse to the same prefix, as do two branches that differ only past 96 chars); the hash of the untouched name restores collision-resistance so two distinct branches never share a managed directory.

The resulting layout is `<managed-root>/<safe(project)>/<safe(branch)>/`, where `safe(x) = <sanitized-truncated-prefix>-<fnv16hex>`. The decision to keep worktrees in a managed global directory is unchanged; only the per-component encoding is amended. This pairs with ADR 0012's `\\?\` extended-length filesystem prefixing as Hitch's two-part MAX_PATH mitigation.
