# Plan: Draft Generator for Commit and PR Text

## Problem

Users currently must write commit messages, commit bodies, PR titles, and PR descriptions manually even though Hitch already has the relevant git context. They need fast, editable draft generation for common git flows without changing Hitch's terminal-native Agent model or making commit/PR forms permanently occupy the right rail.

## Solution

Add a daemon-owned **Draft Generator** that produces non-interactive drafts from git context. It is separate from **Agent** and **Agent State**: Agents remain known CLIs running in PTY Sessions, while draft generation is a headless daemon action behind explicit IPC requests.

Move commit and PR authoring into overlays. The right rail remains focused on changed files and contextual git actions:

- On the default branch: show **Commit…** and **Push**.
- On non-default branches: show **Commit…**, **Push**, and **Create PR…**.

Commit overlay fields are **subject** and **body**. PR overlay fields are **title**, **body**, **base branch**, and **draft**. Generation is explicit via **Generate** / **Regenerate**, preserves user edits unless confirmed, and keeps fields editable after errors.

Ship the first Draft Provider as a deterministic stub provider so the complete app flow can be built and tested before wiring a real headless CLI provider.

## User Stories

1. As a developer, I want Hitch to draft a commit subject and body from staged changes, so that I can commit faster while still reviewing the text.
2. As a developer, I want Hitch to offer **Stage all & generate** when the worktree is dirty but nothing is staged, so that I can quickly start a commit draft without silent auto-staging.
3. As a developer, I want Hitch to draft a PR title and body from branch-level context, so that opening a PR is faster and less repetitive.
4. As a developer, I want generated text to be editable and not overwrite my edits without confirmation, so that generation remains assistive rather than destructive.
5. As a developer, I want commit and PR forms in overlays, so that the changes panel stays compact and focused on file state.

## Implementation Decisions

- Domain/docs:
  - `CONTEXT.md` now defines **Draft Generator** and resolves "Agent harness" as an avoided term.
  - `docs/adr/0007-draft-generator-is-not-agent-state.md` records that draft generation is separate from Agent Sessions and Agent State.
- Architecture:
  - The daemon owns draft generation and git-context composition.
  - `src-tauri` stays a thin IPC client.
  - `hitch-git` remains focused on git operations and helpers, not draft-provider policy.
  - Draft Providers are separate from the Agent Registry and hook installation.
- Protocol:
  - Add explicit IPC requests such as `generate-commit-draft` and `generate-pull-request-draft`.
  - Add explicit response payloads:
    - `CommitDraft { subject, body }`
    - `PullRequestDraft { title, body }`
- Git context:
  - Commit generation uses staged diff only.
  - If no files are staged but the worktree is dirty, the commit overlay offers **Stage all & generate**.
  - PR generation uses branch-level context relative to the selected base branch: commits on the branch plus diff from base.
  - If PR base cannot be determined or entered, generation errors inline and asks for a base branch.
- Commit behavior:
  - Commit is enabled when subject is non-empty and staged files exist.
  - Body is optional.
  - Use the system `git` CLI with a temp message file, e.g. `git commit -F <file>`, so subject/body formatting is preserved while hooks, signing, config, and credential behavior stay faithful to terminal git.
- PR behavior:
  - Generate both title and body.
  - PR body includes a `## Summary` section and a `## Testing` section with `- [ ] Not run` in the stub implementation.
  - PR creation keeps current behavior: backend pushes first if needed, then runs `gh pr create`.
  - PR draft generation does not require the branch to be pushed.
- UI behavior:
  - Generation is explicit; overlays do not auto-generate on open.
  - If target fields are non-empty, generation asks for confirmation before replacing them.
  - On generation failure, keep existing field values, show inline error, and allow manual commit/PR creation.
  - Drafts are overlay-local and not persisted.
  - No provider settings UI in the first implementation.
- Stub provider:
  - Commit subject uses a deterministic Conventional-style subject such as `chore: update <primary area>`.
  - Commit body includes bullets derived from staged file paths.
  - PR title derives from branch/context.
  - PR body includes changed files, commit summaries, and testing placeholder.

## Testing Decisions

- Add protocol round-trip tests for the new request and response variants.
- Add daemon integration tests for generating commit and PR drafts through the socket.
- Add git tests for committing with subject/body via `git commit -F <tempfile>`, including multiline bodies and hook preservation.
- Add frontend tests or type-check coverage for overlay state where practical; at minimum run existing Svelte/TypeScript checks.
- Manually verify:
  - Default branch hides **Create PR…**.
  - Non-default branch shows **Create PR…**.
  - Commit overlay blocks commit with no staged files.
  - Dirty/no-staged state offers **Stage all & generate**.
  - Replace confirmation appears when fields already contain text.
  - Generation errors do not clear fields.

## Slices

1. Protocol and draft domain contract
   - Goal: add stable IPC shapes for draft generation.
   - Areas: `crates/hitch-proto/src/message.rs`, protocol catalog/tests, TS request/response typing in `apps/desktop/src/lib/daemon.ts` as needed.
   - Acceptance: proto round-trip tests include `GenerateCommitDraft`, `GeneratePullRequestDraft`, `CommitDraft`, and `PullRequestDraft`.
   - Depends on: none.
   - Parallel: yes, with slice 2 after request/response names are agreed.

2. Git commit subject/body execution
   - Goal: support full commit messages from separate subject/body fields.
   - Areas: `crates/hitch-git/src/lib.rs`, daemon commit handler, existing commit tests.
   - Acceptance: commit uses system git with full message preserved; existing hook/signing-fidelity tests still pass; multiline body is committed correctly.
   - Depends on: none.
   - Parallel: yes.

3. Draft Generator backend and stub provider
   - Goal: implement daemon-owned generation using deterministic git context.
   - Areas: new/updated daemon module in `crates/hitch-daemon`, git context helpers in `hitch-git` if needed, daemon socket handlers.
   - Acceptance: daemon tests can request commit and PR drafts; commit draft uses staged diff only; PR draft requires base branch; stub output is deterministic.
   - Depends on: slice 1.
   - Parallel: no.

4. Commit overlay UI
   - Goal: replace always-visible commit textarea with a **Commit…** overlay.
   - Areas: `apps/desktop/src/lib/components/RightRail.svelte`, new `CommitDialog.svelte`, `apps/desktop/src/lib/daemon.ts`, overlay state.
   - Acceptance: right rail opens commit overlay; subject/body are editable; **Generate** works; **Stage all & generate** appears only when dirty and nothing is staged; commit requires staged files and non-empty subject.
   - Depends on: slices 1-3 for live generation; can start earlier against stubbed frontend calls.
   - Parallel: yes, after slice 1 contract.

5. PR overlay generation and contextual actions
   - Goal: update PR overlay and right rail actions for dynamic branch behavior.
   - Areas: `CreatePrDialog.svelte`, `RightRail.svelte`, `daemon.ts`.
   - Acceptance: non-default branches show **Create PR…**; default branch hides it; PR overlay can generate title/body from base; missing base shows inline error; existing create-PR behavior still works.
   - Depends on: slices 1 and 3.
   - Parallel: yes with slice 4.

6. Polish, docs, and regression pass
   - Goal: tighten edge cases and document the new flow.
   - Areas: `CONTEXT.md`, ADR cross-checks, UI copy, test suite.
   - Acceptance: `cargo test` passes; frontend check/build passes; docs use **Draft Generator** consistently and avoid "Agent harness".
   - Depends on: all implementation slices.
   - Parallel: no.

Parallel groups: [1, 2], [3], [4, 5], [6].

## Out of Scope

- Real Claude/Codex/headless provider integration.
- Provider configuration UI.
- Persisting drafts across overlay close/reopen.
- Automatic generation on overlay open.
- Hunk-level staging or advanced source-control workflows.
- Expanding **Agent State** to cover draft generation.

## Further Notes

The first implementation should optimize for a correct seam: daemon-owned generation, explicit protocol, editable overlays, and deterministic tests. A real headless CLI provider can be added later behind the Draft Provider abstraction once invocation contract, auth behavior, streaming, timeouts, and error mapping are verified.
