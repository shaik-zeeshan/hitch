<script lang="ts">
  // Right rail — Changes (mockup .rail-right). Staged / unstaged file groups
  // with inline stage toggles, a commit box, and Push / Create-PR. Clicking a
  // file row opens its diff (the diff tab itself lands in slice 7; here it sets
  // the selection + loads the diff text). Branch-level +/− stats live in the
  // tree; this panel focuses on file status and commit actions.
  import { DropdownMenu } from "bits-ui";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import {
    cancelJob,
    cancellableJobForSelectedWorktree,
    commit,
    defaultBase,
    diffPath,
    discardAllFiles,
    discardFile,
    generateCommitDraft,
    gitBusy,
    gitStatus,
    gitWorktreeId,
    loadGitStatus,
    loadPrStatus,
    prInfo,
    pull,
    push,
    setFileStaged,
    setFilesStaged,
    viewDiff,
  } from "../daemon";
  import { autoCommitPush } from "../settings";
  import { commitOpen, createPrOpen } from "../overlays";
  import { STATUS_GLYPH, statusGlyphClass } from "../types";
  import CommitDialog from "./CommitDialog.svelte";
  import CreatePrDialog from "./CreatePrDialog.svelte";
  import toast from "svelte-french-toast";

  let {
    collapsed = false,
    onToggleRight,
  }: {
    collapsed?: boolean;
    onToggleRight: () => void;
  } = $props();

  const files = $derived($gitStatus?.files ?? []);
  const staged = $derived(files.filter((f) => f.staged));
  const unstaged = $derived(files.filter((f) => !f.staged));
  const ahead = $derived($gitStatus?.ahead ?? 0);
  const behind = $derived($gitStatus?.behind ?? 0);
  const isDefaultBranch = $derived(Boolean($defaultBase && $gitStatus?.branch === $defaultBase));

  const cancellableJob = $derived($cancellableJobForSelectedWorktree);

  let autoRunning = $state(false);

  // ---- smart actions ------------------------------------------------------
  // One state machine drives both the split button's primary action and the
  // enabled/disabled state of every item in its dropdown. The primary always
  // does the *next* meaningful step; the dropdown exposes each step directly,
  // greyed out (with a reason) when it doesn't apply to the current status.
  const pr = $derived($prInfo);
  const hasChanges = $derived(files.length > 0);
  const onDefault = $derived(isDefaultBranch);
  const busy = $derived($gitBusy || autoRunning);

  function openCommit() {
    commitOpen.set(true);
  }
  function openCreatePr() {
    createPrOpen.set(true);
  }
  async function openPr() {
    if (pr) await openUrl(pr.url);
  }

  // The headline action: the first applicable step in commit → pull → push →
  // create-PR → open-PR order. `null` run means nothing to do (e.g. clean +
  // synced on the default branch) and the button renders disabled.
  type PrimaryAction = { label: string; run: (() => void) | null };
  const primary = $derived<PrimaryAction>(
    hasChanges
      ? $autoCommitPush
        ? { label: "Commit & Push", run: () => void handleAutoCommitPush() }
        : { label: "Commit…", run: openCommit }
      : behind > 0
        ? { label: `Pull ↓${behind}`, run: () => void handleManualPull() }
        : ahead > 0
          ? { label: `Push ↑${ahead}`, run: () => void handleManualPush() }
          : !onDefault && $gitWorktreeId && !pr
            ? { label: "Create PR", run: openCreatePr }
            : pr
              ? { label: `Open PR #${pr.number}`, run: () => void openPr() }
              : { label: "Up to date", run: null },
  );

  // Per-step availability + the reason shown when a step is unavailable, so the
  // dropdown reads as a checklist of what this worktree can do right now.
  const pushReason = $derived(ahead > 0 ? "" : "Nothing to push");
  const pullReason = $derived(behind > 0 ? "" : "Up to date with remote");
  const commitReason = $derived(hasChanges ? "" : "No changes to commit");
  const createPrReason = $derived(
    onDefault ? "On the default branch" : !$gitWorktreeId ? "No worktree selected" : "",
  );

  function shortError(err: unknown): string {
    const msg = err instanceof Error ? err.message : String(err);
    const first = msg.split("\n")[0].trim();
    return first.length > 80 ? first.slice(0, 77) + "…" : first;
  }

  async function handleAutoCommitPush() {
    if ($gitBusy || autoRunning) return;
    autoRunning = true;
    const id = toast.loading("Staging files…");
    try {
      if (unstaged.length > 0) {
        await setFilesStaged(unstaged.map((f) => f.path), true);
      }
      toast.loading("Generating commit message…", { id });
      const draft = await generateCommitDraft();
      toast.loading("Committing…", { id });
      await commit(draft.subject, draft.body);
      toast.loading("Pushing…", { id });
      await push();
      if ($gitStatus?.worktree_id) {
        void loadGitStatus($gitStatus.worktree_id).catch(() => {});
        void loadPrStatus($gitStatus.worktree_id);
      }
      toast.success(draft.subject, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    } finally {
      autoRunning = false;
    }
  }

  async function handleManualPush() {
    const count = ahead;
    const id = toast.loading("Pushing…");
    try {
      await push();
      if ($gitStatus?.worktree_id) {
        void loadGitStatus($gitStatus.worktree_id).catch(() => {});
        void loadPrStatus($gitStatus.worktree_id);
      }
      toast.success(`Pushed ↑${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  async function handleManualPull() {
    const count = behind;
    const id = toast.loading("Pulling…");
    try {
      await pull();
      if ($gitStatus?.worktree_id) void loadGitStatus($gitStatus.worktree_id).catch(() => {});
      toast.success(`Pulled ↓${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  function confirmDiscardAll() {
    if (files.length === 0 || $gitBusy) return;
    if (window.confirm(`Discard all ${files.length} changed file${files.length === 1 ? "" : "s"}?`)) {
      void discardAllFiles();
    }
  }

  function confirmDiscardFile(path: string) {
    if ($gitBusy) return;
    if (window.confirm(`Discard changes to ${path}?`)) {
      void discardFile(path);
    }
  }
</script>

<aside class="rail-right" class:collapsed>
  <div class="ch-head">
    <span class="title">Changes</span>
    <span class="grow"></span>
    <button
      class="iconbtn"
      title="Refresh"
      aria-label="Refresh status"
      disabled={!$gitWorktreeId}
      onclick={() => {
        if (!$gitWorktreeId) return;
        void loadGitStatus($gitWorktreeId);
        void loadPrStatus($gitWorktreeId);
      }}
    >
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
        ><path d="M13 8a5 5 0 1 1-1.5-3.6M13 2.5V5h-2.5" /></svg
      >
    </button>
    <button
      class="iconbtn"
      title="Hide changes panel"
      aria-label="Hide changes panel"
      onclick={onToggleRight}
    >
      <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
        ><path d="M6.5 4.5 10 8l-3.5 3.5" /></svg
      >
    </button>
  </div>

  {#if $gitStatus}
    <div class="ch-branch">
      <svg class="branch-ico" width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="var(--tx-lo)" stroke-width="1.3"
        ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle cx="12" cy="6" r="1.8" /><path
          d="M4 5.8v4.4M5.7 4.5c3 0 4.6 0 5.3 0M11 7.7c0 1.5-1.4 2.4-3.2 2.4H5.8"
        /></svg
      >
      <span class="b" title={$gitStatus.branch}>{$gitStatus.branch}</span>
      {#if $defaultBase && $defaultBase !== $gitStatus.branch}
        <span class="from">from {$defaultBase}</span>
      {/if}
      {#if pr}
        <a
          class="pr-badge {pr.draft ? 'draft' : pr.state.toLowerCase()}"
          href={pr.url}
          title="{pr.draft ? 'Draft' : pr.state} pull request #{pr.number} — open on GitHub"
          onclick={(e) => {
            e.preventDefault();
            void openPr();
          }}>#{pr.number}</a
        >
      {/if}
    </div>

    <div class="ch-actions">
      {#if cancellableJob}
        <button
          class="splitbtn solo cancel"
          title="Cancel the running operation"
          onclick={() => void cancelJob(cancellableJob.id)}
        >
          Cancel
        </button>
      {:else}
        <div class="splitbtn">
          <button
            class="split-main"
            disabled={!primary.run || busy}
            onclick={() => primary.run?.()}
          >
            {primary.label}
          </button>
          <DropdownMenu.Root>
            <DropdownMenu.Trigger>
              {#snippet child({ props })}
                <button
                  {...props}
                  class="split-caret"
                  aria-label="More git actions"
                  title="More git actions"
                >
                  <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.6" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M4 6l4 4 4-4" /></svg
                  >
                </button>
              {/snippet}
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="menu act-menu" align="end" side="bottom" sideOffset={6}>
                <DropdownMenu.Item class="mi" disabled={!hasChanges} title={commitReason} onSelect={openCommit}>
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><circle cx="8" cy="8" r="2.4" /><path d="M1.5 8h4.1M10.4 8h4.1" /></svg
                  >
                  Commit…
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi"
                  disabled={!hasChanges || busy}
                  title={commitReason}
                  onSelect={() => void handleAutoCommitPush()}
                >
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                    ><circle cx="5" cy="8" r="2.2" /><path d="M7.2 8H10M10 5.5 12.5 8 10 10.5" /></svg
                  >
                  Commit &amp; Push
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                <DropdownMenu.Item
                  class="mi"
                  disabled={ahead === 0 || busy}
                  title={pushReason}
                  onSelect={() => void handleManualPush()}
                >
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M8 13V4M4.5 7.5 8 4l3.5 3.5" /></svg
                  >
                  Push <span class="mi-k">↑{ahead}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi"
                  disabled={behind === 0 || busy}
                  title={pullReason}
                  onSelect={() => void handleManualPull()}
                >
                  <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"
                    ><path d="M8 3v9M4.5 8.5 8 12l3.5-3.5" /></svg
                  >
                  Pull <span class="mi-k">↓{behind}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                {#if pr}
                  <DropdownMenu.Item class="mi" onSelect={() => void openPr()}>
                    <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                      ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle cx="12" cy="12" r="1.8" /><path
                        d="M4 5.8v4.4M12 5.5v4.7M12 5.5c0-2-1.6-2.5-3.2-2.5H6"
                      /></svg
                    >
                    Open PR #{pr.number} <span class="mi-k">↗</span>
                  </DropdownMenu.Item>
                {:else}
                  <DropdownMenu.Item
                    class="mi"
                    disabled={Boolean(createPrReason)}
                    title={createPrReason}
                    onSelect={openCreatePr}
                  >
                    <svg class="mi-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round"
                      ><path d="M8 4v8M4 8h8" /></svg
                    >
                    Create PR…
                  </DropdownMenu.Item>
                {/if}
                <DropdownMenu.Separator class="m-sep" />
                <DropdownMenu.Item
                  class="mi toggle"
                  closeOnSelect={false}
                  onSelect={() => autoCommitPush.update((v) => !v)}
                >
                  <span class="check" class:on={$autoCommitPush} aria-hidden="true">✓</span>
                  Auto-generate commit message
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>
      {/if}
    </div>
  {/if}

  <div class="ch-list">
    {#if !$gitWorktreeId}
      <div class="empty"><p>Select a git worktree to see its changes.</p></div>
    {:else if files.length === 0}
      <div class="empty"><p>Working tree clean.<br />Nothing to commit.</p></div>
    {:else}
      {#if staged.length > 0}
        <div class="grp-head">
          Staged <span class="n">{staged.length}</span>
          <span class="acts">
            <button
              class="act"
              onclick={() => void setFilesStaged(staged.map((f) => f.path), false).catch(() => {})}
              >Unstage all</button
            >
          </span>
        </div>
        {#each staged as file (file.path)}
          <button class="frow" class:sel={$diffPath === file.path} onclick={() => void viewDiff(file.path)}>
            <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
            <span class="fp">{file.path}</span>
            <span
              class="stage"
              role="button"
              tabindex="-1"
              title="Unstage"
              aria-label="Unstage {file.path}"
              onclick={(e) => {
                e.stopPropagation();
                void setFileStaged(file.path, false).catch(() => {});
              }}
              onkeydown={() => {}}>−</span
            >
            <span
              class="stage discard"
              role="button"
              tabindex="-1"
              title="Discard file"
              aria-label="Discard changes to {file.path}"
              onclick={(e) => {
                e.stopPropagation();
                confirmDiscardFile(file.path);
              }}
              onkeydown={() => {}}>×</span
            >
          </button>
        {/each}
      {/if}

      {#if unstaged.length > 0}
        <div class="grp-head">
          Changes <span class="n">{unstaged.length}</span>
          <span class="acts">
            <button
              class="act"
              onclick={() => void setFilesStaged(unstaged.map((f) => f.path), true).catch(() => {})}
              >Stage all</button
            >
            <span class="sep" aria-hidden="true">·</span>
            <button class="act danger" disabled={$gitBusy} onclick={confirmDiscardAll}>Discard</button>
          </span>
        </div>
        {#each unstaged as file (file.path)}
          <button class="frow" class:sel={$diffPath === file.path} onclick={() => void viewDiff(file.path)}>
            <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
            <span class="fp">{file.path}</span>
            <span
              class="stage"
              role="button"
              tabindex="-1"
              title="Stage"
              aria-label="Stage {file.path}"
              onclick={(e) => {
                e.stopPropagation();
                void setFileStaged(file.path, true).catch(() => {});
              }}
              onkeydown={() => {}}>+</span
            >
            <span
              class="stage discard"
              role="button"
              tabindex="-1"
              title="Discard file"
              aria-label="Discard changes to {file.path}"
              onclick={(e) => {
                e.stopPropagation();
                confirmDiscardFile(file.path);
              }}
              onkeydown={() => {}}>×</span
            >
          </button>
        {/each}
      {/if}
    {/if}
  </div>

  <!-- Mounted once, triggerless: opened from the action menu (and the ⌘K
       palette) via the commitOpen / createPrOpen stores. -->
  <CommitDialog triggerless />
  <CreatePrDialog triggerless />
</aside>

<style>
  .rail-right {
    background: var(--bg-2);
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    transition: opacity var(--t);
  }
  .rail-right.collapsed {
    opacity: 0;
    pointer-events: none;
  }

  .ch-head {
    flex: none;
    display: flex;
    align-items: center;
    gap: 4px;
    padding: 9px 10px 8px 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .ch-head .title {
    font-size: 12.5px;
    font-weight: 600;
    color: var(--tx-hi);
  }
  .ch-head .grow {
    flex: 1;
  }

  /* Branch row — its own line so a long name truncates instead of crowding
     the actions (which now live on the row below). */
  .ch-branch {
    flex: none;
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    padding: 7px 10px 5px 12px;
    font-size: 11px;
    color: var(--tx-md);
  }
  .ch-branch .branch-ico {
    flex: none;
  }
  .ch-branch .b {
    flex: 0 1 auto;
    min-width: 0;
    font-family: var(--mono);
    color: var(--tx-hi);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .ch-branch .from {
    flex: none;
    color: var(--tx-lo);
    white-space: nowrap;
  }
  .pr-badge {
    flex: none;
    margin-left: auto;
    font-family: var(--mono);
    font-size: 10px;
    font-weight: 600;
    padding: 1px 6px;
    border-radius: 99px;
    text-decoration: none;
    border: 1px solid var(--line);
    background: var(--bg-3);
    color: var(--tx-md);
    transition: background var(--t-fast);
  }
  .pr-badge.open {
    color: var(--ok);
    border-color: oklch(62% 0.14 150 / 0.4);
  }
  .pr-badge.merged {
    color: oklch(72% 0.13 300);
    border-color: oklch(60% 0.14 300 / 0.4);
  }
  .pr-badge.closed {
    color: var(--err);
    border-color: oklch(58% 0.14 25 / 0.4);
  }
  .pr-badge.draft {
    color: var(--tx-lo);
  }
  .pr-badge:hover {
    background: var(--bg-4);
  }

  /* Action row — a state-driven split button: the primary does the next
     meaningful step, the caret opens the full menu of granular actions. */
  .ch-actions {
    flex: none;
    padding: 0 10px 9px 12px;
    border-bottom: 1px solid var(--line-soft);
  }
  .splitbtn {
    display: flex;
    align-items: stretch;
    width: 100%;
    border: 1px solid var(--line);
    border-radius: var(--radius);
    background: var(--bg-3);
    overflow: hidden;
  }
  .split-main {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 11.5px;
    font-weight: 500;
    padding: 5px 10px;
    text-align: center;
    color: var(--tx-hi);
    background: transparent;
    border: 0;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: background var(--t-fast);
  }
  .split-main:hover:not(:disabled) {
    background: var(--bg-4);
  }
  .split-main:disabled {
    color: var(--tx-lo);
    cursor: default;
  }
  .split-caret {
    flex: none;
    display: grid;
    place-items: center;
    width: 26px;
    border: 0;
    border-left: 1px solid var(--line);
    background: transparent;
    color: var(--tx-md);
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .split-caret:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .split-caret svg {
    width: 13px;
    height: 13px;
  }
  /* Cancel state replaces the whole split with one destructive button. */
  .splitbtn.solo {
    font: inherit;
    font-size: 11.5px;
    font-weight: 500;
    padding: 5px 10px;
    justify-content: center;
    cursor: pointer;
    color: oklch(80% 0.08 25);
    border-color: oklch(58% 0.14 25 / 0.4);
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .splitbtn.solo.cancel:hover {
    color: var(--err);
    background: oklch(58% 0.14 25 / 0.12);
  }

  /* Disabled menu items read as "unavailable, here's why" (title tooltip). */
  :global(.act-menu .mi[data-disabled]) {
    opacity: 0.4;
    pointer-events: none;
  }
  :global(.act-menu .mi .check) {
    width: 14px;
    flex: none;
    text-align: center;
    color: transparent;
  }
  :global(.act-menu .mi .check.on) {
    color: var(--ok);
  }

  .ch-list {
    flex: 1;
    overflow-y: auto;
    min-height: 0;
    padding: 6px 6px 10px;
  }
  .empty {
    padding: 38px 20px;
    text-align: center;
  }
  .empty p {
    font-size: 12px;
    color: var(--tx-lo);
    line-height: 1.55;
  }

  .grp-head {
    display: flex;
    align-items: center;
    gap: 6px;
    padding: 9px 6px 5px;
    font-size: 10.5px;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.5px;
    color: var(--tx-lo);
  }
  .grp-head .n {
    color: var(--tx-md);
  }
  .grp-head .acts {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .grp-head .sep {
    color: var(--tx-lo);
    opacity: 0.5;
  }
  .grp-head .act {
    font: inherit;
    font-size: 10.5px;
    text-transform: none;
    letter-spacing: 0;
    color: var(--ac);
    cursor: pointer;
    background: transparent;
    border: 0;
    padding: 0;
    transition: color var(--t-fast);
  }
  .grp-head .act:hover {
    color: var(--ac-bright);
  }
  .grp-head .act.danger {
    color: var(--tx-lo);
  }
  .grp-head .act.danger:hover {
    color: var(--err);
  }
  .grp-head .act:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .grp-head .act.danger:disabled:hover {
    color: var(--tx-lo);
  }

  .frow {
    display: flex;
    align-items: center;
    gap: 8px;
    width: 100%;
    text-align: left;
    font: inherit;
    padding: 6px 8px;
    border-radius: var(--radius);
    color: var(--tx-md);
    cursor: pointer;
    background: transparent;
    border: 1px solid transparent;
    transition: background var(--t-fast);
  }
  .frow:hover {
    background: var(--bg-3);
  }
  .frow.sel {
    background: var(--ac-wash);
    color: var(--tx-hi);
    box-shadow: inset 0 0 0 1px oklch(62% 0.1 265 / 0.28);
  }
  .frow .st {
    width: 14px;
    text-align: center;
    font-family: var(--mono);
    font-size: 11px;
    font-weight: 700;
    flex: none;
  }
  .frow .st.M {
    color: var(--warn);
  }
  .frow .st.A {
    color: var(--ok);
  }
  .frow .st.D {
    color: var(--err);
  }
  .frow .st.U {
    color: var(--tx-lo);
  }
  .frow .fp {
    flex: 1;
    min-width: 0;
    font-family: var(--mono);
    font-size: 11.5px;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    direction: rtl;
    text-align: left;
  }
  .frow .stage {
    opacity: 0;
    width: 18px;
    height: 18px;
    border-radius: 4px;
    display: grid;
    place-items: center;
    color: var(--tx-md);
    flex: none;
  }
  .frow:hover .stage,
  .frow.sel .stage {
    opacity: 1;
  }
  .frow .stage:hover {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  .frow .discard:hover {
    color: var(--err);
  }

  .iconbtn {
    width: 26px;
    height: 26px;
    display: grid;
    place-items: center;
    border-radius: var(--radius);
    color: var(--tx-md);
    border: 1px solid transparent;
    background: transparent;
    cursor: pointer;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  .iconbtn:hover {
    background: var(--bg-3);
    color: var(--tx-hi);
  }
  .iconbtn:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .iconbtn svg {
    width: 15px;
    height: 15px;
  }
</style>
