# Composer replaces git dialogs; PR creation is autonomous draft-PR

The right rail's smart action no longer opens modal dialogs (CommitDialog / CreatePrDialog are deleted). Both flows move into the **Composer**, an anchored overlay card in the rail — it floats over the file list below the action button with no backdrop, so the rail never reflows and the user's attention stays on the center agent panes. The driving constraint: Hitch users are babysitting AI agents; a modal that steals focus for git bookkeeping is exactly the attention tax the app exists to remove.

The two flows deliberately sit at **different autonomy levels**:

- **Commit = glance-and-confirm.** Opening the Composer starts draft generation immediately (from the staged diff if anything is staged, otherwise it stages all first); the draft fills an editable subject/body and Enter commits. Enter during generation queues commit-on-arrival (Esc cancels; a failed generation cancels the queue — a fallback message is never auto-committed). Commits are local and cheap to amend, so a glance is enough.
- **Create PR = autonomous.** A one-row pre-flight (base-branch select prefilled with the default base) is the only input; after confirm the flow runs hands-off: push → generate title/body → create the PR **as a GitHub draft** → open it in the browser. Title/body are never reviewed locally — the GitHub draft flag is the review gate, and the browser (where attention is going anyway) is the review surface. Creating it as a draft suppresses reviewer notifications, so a bad AI body never pings teammates.

## Considered Options

- **Composer for both, autonomous draft-PR** (chosen).
- **Glance-and-confirm for PRs too** — rejected: PR bodies are long, the rail is narrow, and the user reviews on GitHub anyway; a local review step duplicates that.
- **Fully autonomous commit (no composer)** — rejected as the *default*; it survives as the existing opt-in auto-commit-push toggle, which bypasses the Composer entirely. Users graduate to it when they trust the drafts.
- **In-flow expansion instead of overlay** — rejected: it reflows the file list under the user's eyes (layout shift was an explicit objection).

## Amendment (2026-06-07): autonomous chains are daemon-owned composite Jobs

The two autonomous chains — auto commit & push (the existing toggle) and the PR create flow (push → generate → create draft PR) — run as single daemon-side composite Job kinds (*commit-and-push*, *create-pr*), not as GUI-orchestrated sequences of individual Jobs. Rationale: GUI orchestration strands the chain when the component unmounts or the app quits (e.g. committed but never pushed, with no explanation), and its progress state is component-local, so returning to the rail mid-chain showed an idle button while work was in flight. As composite Jobs the chain completes regardless of GUI lifetime, progress is broadcast per step as the Job's events, and an attaching GUI queries a worktree's active Jobs to restore the Composer/button display exactly. This extends ADR 0008 with an "active Jobs by worktree" query; Jobs remain ephemeral across daemon restarts. The chain aborts before commit if generation fails (a fallback message is never auto-committed); the browser-open finale of the PR chain is a completion event the GUI acts on only if attached — otherwise the rail's PR chip reflects the created PR on next attach. Auto mode shows chain progress by morphing the action button's label in place (staging → drafting → committing → pushing → subject), never opening the Composer body.

## Consequences

- PRs always start as GitHub drafts; "ready for review" is clicked on GitHub.
- The autonomous PR path implies auto-push before creation.
- Cmd/Ctrl+Enter and command-palette entries retarget from dialogs to the Composer.
- Drafts arrive whole (the Job returns one JSON blob); the Composer shows a generating state, not token streaming — streaming would be a separate daemon protocol change.
