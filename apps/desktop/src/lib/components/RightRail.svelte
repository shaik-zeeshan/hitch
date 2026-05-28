<script lang="ts">
  // Right rail — Changes (mockup .rail-right). Staged / unstaged file groups
  // with inline stage toggles, a commit box, and Push / Create-PR. Clicking a
  // file row opens its diff (the diff tab itself lands in slice 7; here it sets
  // the selection + loads the diff text). Branch-level +/− stats live in the
  // tree; this panel focuses on file status and commit actions.
  import {
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
    pull,
    push,
    setFileStaged,
    setFilesStaged,
    viewDiff,
  } from "../daemon";
  import { autoCommitPush } from "../settings";
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

  let autoRunning = $state(false);

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
      if ($gitStatus?.worktree_id) void loadGitStatus($gitStatus.worktree_id).catch(() => {});
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
      if ($gitStatus?.worktree_id) void loadGitStatus($gitStatus.worktree_id).catch(() => {});
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
      onclick={() => $gitWorktreeId && void loadGitStatus($gitWorktreeId)}
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
      <svg width="13" height="13" viewBox="0 0 16 16" fill="none" stroke="var(--tx-lo)" stroke-width="1.3"
        ><circle cx="4" cy="4" r="1.8" /><circle cx="4" cy="12" r="1.8" /><circle cx="12" cy="6" r="1.8" /><path
          d="M4 5.8v4.4M5.7 4.5c3 0 4.6 0 5.3 0M11 7.7c0 1.5-1.4 2.4-3.2 2.4H5.8"
        /></svg
      >
      <span class="b">{$gitStatus.branch}</span>
      {#if $defaultBase && $defaultBase !== $gitStatus.branch}
        <span class="from">from {$defaultBase}</span>
      {/if}
      <span class="branch-acts">
        {#if files.length > 0}
          {#if $autoCommitPush}
            <button
              class="chip"
              disabled={$gitBusy || autoRunning}
              onclick={() => void handleAutoCommitPush()}
            >
              Commit & Push
            </button>
          {:else}
            <CommitDialog disabled={$gitBusy} triggerClass="chip" />
          {/if}
        {/if}
        {#if behind > 0}
          <button class="chip" disabled={$gitBusy} onclick={() => void handleManualPull()}>
            Pull <span class="ar down">↓{behind}</span>
          </button>
        {/if}
        {#if ahead > 0 && !$autoCommitPush}
          <button class="chip" disabled={$gitBusy} onclick={() => void handleManualPush()}>
            Push <span class="ar">↑{ahead}</span>
          </button>
        {/if}
        {#if !isDefaultBranch && $gitWorktreeId}
          <CreatePrDialog disabled={$gitBusy} triggerClass="chip" />
        {/if}
      </span>
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

  .ch-branch {
    flex: none;
    display: flex;
    align-items: center;
    gap: 7px;
    padding: 7px 10px 7px 12px;
    border-bottom: 1px solid var(--line-soft);
    font-size: 11px;
    color: var(--tx-md);
  }
  .ch-branch .b {
    font-family: var(--mono);
    color: var(--tx-hi);
  }
  .ch-branch .from {
    color: var(--tx-lo);
  }
  .chip .ar.down {
    color: var(--warn);
  }
  .branch-acts {
    margin-left: auto;
    display: flex;
    align-items: center;
    gap: 4px;
    flex-shrink: 0;
  }
  :global(.chip) {
    font: inherit;
    font-size: 10.5px;
    padding: 2px 7px;
    border-radius: 99px;
    border: 1px solid var(--line);
    background: var(--bg-3);
    color: var(--tx-md);
    cursor: pointer;
    white-space: nowrap;
    transition:
      background var(--t-fast),
      color var(--t-fast);
  }
  :global(.chip:hover) {
    background: var(--bg-4);
    color: var(--tx-hi);
  }
  :global(.chip:disabled) {
    opacity: 0.4;
    cursor: default;
  }
  .chip .ar {
    color: var(--ok);
    font-family: var(--mono);
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
