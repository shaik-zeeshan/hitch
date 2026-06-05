<script lang="ts">
  // Right rail — Changes (Paper Terminal shell). Header diffstat + branch/PR
  // context, then ONE state-derived split button (the primary always does the
  // next meaningful step; its caret opens the full action menu), then Staged /
  // Changes file groups with inline stage toggles. Clicking a file row opens its
  // diff. This is the restyle of the long-standing smart-action state machine —
  // the ladder below mirrors that logic exactly, it is not a rewrite.
  import { DropdownMenu } from "bits-ui";
  import { openUrl } from "@tauri-apps/plugin-opener";
  import GitBranch from "~icons/lucide/git-branch";
  import GitPullRequest from "~icons/lucide/git-pull-request";
  import GitCommitHorizontal from "~icons/lucide/git-commit-horizontal";
  import ArrowUp from "~icons/lucide/arrow-up";
  import ArrowDown from "~icons/lucide/arrow-down";
  import ArrowUpFromLine from "~icons/lucide/arrow-up-from-line";
  import ArrowUpRight from "~icons/lucide/arrow-up-right";
  import Plus from "~icons/lucide/plus";
  import ChevronDown from "~icons/lucide/chevron-down";
  import RefreshCw from "~icons/lucide/refresh-cw";
  import {
    cancelJob,
    cancellableJobForSelectedWorktree,
    commit,
    defaultBase,
    diffPath,
    discardAllFiles,
    discardFile,
    generateCommitDraft,
    fetchRemote,
    gitBusy,
    gitStatus,
    gitWorktreeId,
    loadGitStatus,
    loadPrStatus,
    openPrInfo,
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

  // `onToggleRight` is kept in the prop contract so the layout wiring stays
  // intact, but the Paper Terminal header drops the hide/collapse toggle (the
  // rail is part of the fixed 3-pane grid). `collapsed` likewise still drives
  // the opacity fade if the layout ever requests it.
  let {
    collapsed = false,
    onToggleRight: _onToggleRight,
  }: {
    collapsed?: boolean;
    onToggleRight: () => void;
  } = $props();

  const files = $derived($gitStatus?.files ?? []);
  const staged = $derived(files.filter((f) => f.staged));
  const unstaged = $derived(files.filter((f) => !f.staged));
  const ahead = $derived($gitStatus?.ahead ?? 0);
  const behind = $derived($gitStatus?.behind ?? 0);
  const additions = $derived($gitStatus?.additions ?? 0);
  const deletions = $derived($gitStatus?.deletions ?? 0);
  const isDefaultBranch = $derived(Boolean($defaultBase && $gitStatus?.branch === $defaultBase));

  const cancellableJob = $derived($cancellableJobForSelectedWorktree);

  let autoRunning = $state(false);

  // ---- smart actions ------------------------------------------------------
  // One state machine drives both the split button's primary action and the
  // enabled/disabled state of every item in its dropdown. The primary always
  // does the *next* meaningful step; the dropdown exposes each step directly,
  // greyed out (with a reason) when it doesn't apply to the current status.
  const pr = $derived($prInfo);
  const openPr = $derived($openPrInfo);
  const hasChanges = $derived(files.length > 0);
  const onDefault = $derived(isDefaultBranch);
  const busy = $derived($gitBusy || autoRunning);

  function openCommit() {
    commitOpen.set(true);
  }
  function openCreatePr() {
    if (busy || !$gitWorktreeId) return;
    createPrOpen.set(true);
  }
  async function openExistingPr() {
    if (openPr) await openUrl(openPr.url);
  }
  async function openDisplayedPr() {
    if (pr) await openUrl(pr.url);
  }

  // The headline action: the first applicable step in commit → pull → push →
  // create-PR → open-PR order. `null` run means nothing to do (e.g. clean +
  // synced on the default branch) and the button renders disabled.
  //
  // `key` names which menu row is currently primary (marked .is-primary); the
  // icon component is chosen per action for the split-button leading glyph.
  type PrimaryKey = "commitpush" | "commit" | "pull" | "push" | "createpr" | "openpr" | "none";
  type PrimaryAction = {
    label: string;
    run: (() => void) | null;
    mutates: boolean;
    key: PrimaryKey;
  };
  const primary = $derived<PrimaryAction>(
    hasChanges
      ? $autoCommitPush
        ? { label: "Commit & Push", run: () => void handleAutoCommitPush(), mutates: true, key: "commitpush" }
        : { label: "Commit…", run: openCommit, mutates: true, key: "commit" }
      : behind > 0
        ? { label: `Pull ↓${behind}`, run: () => void handleManualPull(), mutates: true, key: "pull" }
        : ahead > 0
          ? { label: `Push ↑${ahead}`, run: () => void handleManualPush(), mutates: true, key: "push" }
          : !onDefault && $gitWorktreeId && !openPr
            ? { label: "Create PR", run: openCreatePr, mutates: true, key: "createpr" }
            : openPr
              ? { label: `Open PR #${openPr.number}`, run: () => void openExistingPr(), mutates: false, key: "openpr" }
              : { label: "Up to date", run: null, mutates: false, key: "none" },
  );

  // Per-step availability + the reason shown when a step is unavailable, so the
  // dropdown reads as a checklist of what this worktree can do right now.
  const pushReason = $derived(ahead > 0 ? "" : "Nothing to push");
  const pullReason = $derived(behind > 0 ? "" : "Up to date with remote");
  const commitReason = $derived(busy ? "Git operation in progress" : hasChanges ? "" : "No changes to commit");
  const createPrReason = $derived(
    busy ? "Git operation in progress" : onDefault ? "On the default branch" : !$gitWorktreeId ? "No worktree selected" : "",
  );

  // The "why this action is primary" hint segments, derived from the same state
  // the ladder uses: staged count · ahead/behind · PR status. Quiet, tabular.
  const whyParts = $derived(
    [
      staged.length > 0 ? `${staged.length} staged` : "",
      ahead > 0 ? `↑${ahead}` : "",
      behind > 0 ? `↓${behind}` : "",
      pr ? `PR #${pr.number} ${pr.draft ? "draft" : pr.state.toLowerCase()}` : "",
    ].filter(Boolean),
  );

  function shortError(err: unknown): string {
    const msg = err instanceof Error ? err.message : String(err);
    const first = msg.split("\n")[0].trim();
    return first.length > 80 ? first.slice(0, 77) + "…" : first;
  }

  // Split a path into a dimmed directory part and an emphasized filename.
  function splitPath(path: string): { dir: string; name: string } {
    const idx = path.lastIndexOf("/");
    return idx === -1 ? { dir: "", name: path } : { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
  }

  async function handleAutoCommitPush() {
    const worktreeId = $gitWorktreeId;
    if ($gitBusy || autoRunning || !worktreeId) return;
    const pathsToStage = unstaged.map((file) => file.path);
    autoRunning = true;
    const id = toast.loading("Staging files…");
    try {
      if (pathsToStage.length > 0) {
        await setFilesStaged(pathsToStage, true, worktreeId);
      }
      toast.loading("Generating commit message…", { id });
      const draft = await generateCommitDraft(worktreeId);
      toast.loading("Committing…", { id });
      await commit(draft.subject, draft.body, worktreeId);
      toast.loading("Pushing…", { id });
      await push(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      void loadPrStatus(worktreeId);
      toast.success(draft.subject, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    } finally {
      autoRunning = false;
    }
  }

  async function handleManualPush() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    const count = ahead;
    const id = toast.loading("Pushing…");
    try {
      await push(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      void loadPrStatus(worktreeId);
      toast.success(`Pushed ↑${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  async function handleManualPull() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    const count = behind;
    const id = toast.loading("Pulling…");
    try {
      await pull(worktreeId);
      void loadGitStatus(worktreeId).catch(() => {});
      toast.success(`Pulled ↓${count}`, { id });
    } catch (err) {
      toast.error(shortError(err), { id });
    }
  }

  async function handleRefresh() {
    if ($gitBusy || !$gitWorktreeId) return;
    const worktreeId = $gitWorktreeId;
    const id = toast.loading("Fetching…");
    try {
      await fetchRemote(worktreeId);
      await loadGitStatus(worktreeId);
      void loadPrStatus(worktreeId);
      toast.success("Fetched", { id });
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
  <!-- Header: 38px baseline grid. CHANGES label + net diffstat. A quiet refresh
       icon sits next to the net stat (the old header refresh/hide buttons are
       gone; hide is dropped, refresh lives here + in the action menu's reach). -->
  <div class="changes-head">
    <div class="title">
      <h2>Changes</h2>
      <span class="head-right">
        {#if $gitStatus}
          <span class="net">
            <span class="a">+{additions}</span> <span class="d">−{deletions}</span>
          </span>
        {/if}
        <button
          class="refresh"
          title="Fetch remote and refresh status"
          aria-label="Fetch remote and refresh status"
          disabled={!$gitWorktreeId || $gitBusy}
          onclick={() => void handleRefresh()}
        >
          <RefreshCw class="icon" />
        </button>
      </span>
    </div>
  </div>

  {#if $gitStatus}
    <div class="changes-ctx">
      <div class="branchline">
        <GitBranch class="ic icon" />
        <span class="b" title={$gitStatus.branch}>{$gitStatus.branch}</span>
        {#if $defaultBase && $defaultBase !== $gitStatus.branch}
          <span class="from">from {$defaultBase}</span>
        {/if}
        {#if ahead > 0 || behind > 0}
          <span
            class="ahead"
            title="{ahead} ahead{behind > 0 ? `, ${behind} behind` : ''} of origin"
          >
            {#if ahead > 0}<span class="arr">↑</span>{ahead}{/if}{#if behind > 0}<span class="arr down">↓</span>{behind}{/if}
          </span>
        {/if}
      </div>

      {#if pr}
        <a
          class="pr {pr.draft ? 'draft' : pr.state.toLowerCase()}"
          href={pr.url}
          title="{pr.draft ? 'Draft' : pr.state} pull request #{pr.number} — open on GitHub"
          onclick={(e) => {
            e.preventDefault();
            void openDisplayedPr();
          }}
        >
          <GitPullRequest class="pric icon" />
          <span>PR</span><span class="num">#{pr.number}</span>
        </a>
      {/if}
    </div>

    <div class="actions">
      {#if cancellableJob}
        <button
          class="cancel"
          title="Cancel the running operation"
          onclick={() => void cancelJob(cancellableJob.id)}
        >
          Cancel
        </button>
      {:else}
        <div class="splitbtn" class:disabled={!primary.run}>
          <button
            class="split-main on-iris"
            disabled={!primary.run || (busy && primary.mutates)}
            onclick={() => primary.run?.()}
          >
            {#if primary.key === "commitpush"}
              <ArrowUpFromLine class="btnic icon" />
            {:else if primary.key === "commit"}
              <GitCommitHorizontal class="btnic icon" />
            {:else if primary.key === "push"}
              <ArrowUp class="btnic icon" />
            {:else if primary.key === "pull"}
              <ArrowDown class="btnic icon" />
            {:else if primary.key === "createpr"}
              <Plus class="btnic icon" />
            {:else if primary.key === "openpr"}
              <ArrowUpRight class="btnic icon" />
            {/if}
            {primary.label}
            {#if primary.key === "commit" || primary.key === "commitpush"}
              <kbd>⌘↵</kbd>
            {/if}
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
                  <ChevronDown class="icon" />
                </button>
              {/snippet}
            </DropdownMenu.Trigger>
            <DropdownMenu.Portal>
              <DropdownMenu.Content class="menu act-menu" align="end" side="bottom" sideOffset={6}>
                <DropdownMenu.Item
                  class="mi {primary.key === 'commit' ? 'is-primary' : ''}"
                  disabled={!hasChanges || busy}
                  title={commitReason}
                  onSelect={openCommit}
                >
                  <GitCommitHorizontal class="mi-ico icon" />
                  Commit…
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi {primary.key === 'commitpush' ? 'is-primary' : ''}"
                  disabled={!hasChanges || busy}
                  title={commitReason}
                  onSelect={() => void handleAutoCommitPush()}
                >
                  <ArrowUpFromLine class="mi-ico icon" />
                  Commit &amp; Push <span class="mi-k">⌘↵</span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                <DropdownMenu.Item
                  class="mi {primary.key === 'push' ? 'is-primary' : ''}"
                  disabled={ahead === 0 || busy}
                  title={pushReason}
                  onSelect={() => void handleManualPush()}
                >
                  <ArrowUp class="mi-ico icon" />
                  Push <span class="mi-k">↑{ahead}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Item
                  class="mi {primary.key === 'pull' ? 'is-primary' : ''}"
                  disabled={behind === 0 || busy}
                  title={pullReason}
                  onSelect={() => void handleManualPull()}
                >
                  <ArrowDown class="mi-ico icon" />
                  Pull <span class="mi-k">↓{behind}</span>
                </DropdownMenu.Item>
                <DropdownMenu.Separator class="m-sep" />
                {#if openPr}
                  <DropdownMenu.Item class="mi" onSelect={() => void openExistingPr()}>
                    <GitPullRequest class="mi-ico icon" />
                    Open PR #{openPr.number} <span class="mi-k">↗</span>
                  </DropdownMenu.Item>
                {:else}
                  <DropdownMenu.Item
                    class="mi {primary.key === 'createpr' ? 'is-primary' : ''}"
                    disabled={Boolean(createPrReason)}
                    title={createPrReason}
                    onSelect={openCreatePr}
                  >
                    <Plus class="mi-ico icon" />
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
                  auto-generate commit message
                </DropdownMenu.Item>
              </DropdownMenu.Content>
            </DropdownMenu.Portal>
          </DropdownMenu.Root>
        </div>

        {#if whyParts.length > 0}
          <div class="why-primary">
            {#each whyParts as part, i (part)}
              {#if i > 0}<span class="sep" aria-hidden="true">·</span>{/if}
              <span>{part}</span>
            {/each}
          </div>
        {/if}
      {/if}
    </div>
  {/if}

  <div class="files">
    {#if !$gitWorktreeId}
      <div class="empty"><p>Select a git worktree to see its changes.</p></div>
    {:else if files.length === 0}
      <div class="empty"><p>Working tree clean.<br />Nothing to commit.</p></div>
    {:else}
      {#if staged.length > 0}
        <div class="fgroup">
          <h3>
            <span>Staged</span><span class="ct">{staged.length}</span><span class="hr"></span>
            <button
              class="all"
              onclick={() => void setFilesStaged(staged.map((f) => f.path), false).catch(() => {})}
            >unstage all</button>
          </h3>
          {#each staged as file (file.path)}
            {@const parts = splitPath(file.path)}
            <button class="frow" class:active={$diffPath === file.path} onclick={() => void viewDiff(file.path)}>
              <span
                class="chk on"
                role="button"
                tabindex="-1"
                title="Unstage {file.path}"
                aria-label="Unstage {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  void setFileStaged(file.path, false).catch(() => {});
                }}
                onkeydown={() => {}}
              >✓</span>
              <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
              <span class="path">{#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<b>{parts.name}</b></span>
              <span class="fdiff">
                {#if file.status === "added" || file.status === "untracked"}
                  <span class="a">new</span>
                {/if}
              </span>
              <span
                class="discard"
                role="button"
                tabindex="-1"
                title="Discard file"
                aria-label="Discard changes to {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  confirmDiscardFile(file.path);
                }}
                onkeydown={() => {}}
              >×</span>
            </button>
          {/each}
        </div>
      {/if}

      {#if unstaged.length > 0}
        <div class="fgroup">
          <h3>
            <span>Changes</span><span class="ct">{unstaged.length}</span><span class="hr"></span>
            <button
              class="all"
              onclick={() => void setFilesStaged(unstaged.map((f) => f.path), true).catch(() => {})}
            >stage all</button>
            <button class="all discard-all" disabled={$gitBusy} onclick={confirmDiscardAll}>discard</button>
          </h3>
          {#each unstaged as file (file.path)}
            {@const parts = splitPath(file.path)}
            <button class="frow" class:active={$diffPath === file.path} onclick={() => void viewDiff(file.path)}>
              <span
                class="chk"
                role="button"
                tabindex="-1"
                title="Stage {file.path}"
                aria-label="Stage {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  void setFileStaged(file.path, true).catch(() => {});
                }}
                onkeydown={() => {}}
              ></span>
              <span class="st {statusGlyphClass(file.status)}">{STATUS_GLYPH[file.status]}</span>
              <span class="path">{#if parts.dir}<span class="dir">{parts.dir}</span>{/if}<b>{parts.name}</b></span>
              <span class="fdiff">
                {#if file.status === "added" || file.status === "untracked"}
                  <span class="a">new</span>
                {/if}
              </span>
              <span
                class="discard"
                role="button"
                tabindex="-1"
                title="Discard file"
                aria-label="Discard changes to {file.path}"
                onclick={(e) => {
                  e.stopPropagation();
                  confirmDiscardFile(file.path);
                }}
                onkeydown={() => {}}
              >×</span>
            </button>
          {/each}
        </div>
      {/if}
    {/if}
  </div>

  <div class="rail-r-foot">
    <span><kbd>␣</kbd> stage</span>
    <span><kbd>↵</kbd> open diff</span>
    <span><kbd>⌘↵</kbd> commit</span>
  </div>

  <!-- Mounted once, triggerless: opened from the action menu (and the command
       palette) via the commitOpen / createPrOpen stores. -->
  <CommitDialog triggerless />
  <CreatePrDialog triggerless />
</aside>

<style>
  .rail-right {
    background: var(--paper-1);
    border-left: 1px solid var(--line);
    display: flex;
    flex-direction: column;
    height: 100%;
    min-height: 0;
    overflow: hidden;
    transition: opacity 0.2s ease-out;
  }
  .rail-right.collapsed {
    opacity: 0;
    pointer-events: none;
  }

  /* Header — shares the 38px baseline grid with PROJECTS + the tab strip. */
  .changes-head {
    flex: 0 0 38px;
    height: 38px;
    padding: 0 16px;
    border-bottom: 1px solid var(--line);
  }
  .changes-head .title {
    height: 100%;
    display: flex;
    align-items: center;
    justify-content: space-between;
  }
  .changes-head h2 {
    font-size: 0.6875rem;
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
  }
  .changes-head .head-right {
    display: inline-flex;
    align-items: center;
    gap: 8px;
  }
  .changes-head .net {
    font-family: var(--mono);
    font-size: var(--r0);
    font-variant-numeric: tabular-nums;
  }
  .changes-head .net .a {
    color: var(--diff-add);
    font-weight: 600;
  }
  .changes-head .net .d {
    color: var(--diff-del);
    font-weight: 600;
  }
  .changes-head .refresh {
    display: grid;
    place-items: center;
    width: 18px;
    height: 18px;
    padding: 0;
    border: 0;
    background: transparent;
    color: var(--ink-3);
    cursor: pointer;
    transition: color 0.15s ease-out;
  }
  .changes-head .refresh :global(svg) {
    width: 13px;
    height: 13px;
  }
  .changes-head .refresh:hover:not(:disabled) {
    color: var(--ink-1);
  }
  .changes-head .refresh:disabled {
    opacity: 0.4;
    cursor: default;
  }

  /* Branch + PR context block, directly under the aligned title row. */
  .changes-ctx {
    flex: none;
    padding: 11px 16px 12px;
    border-bottom: 1px solid var(--line);
  }
  .branchline {
    display: flex;
    align-items: center;
    gap: 7px;
    min-width: 0;
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--ink-0);
  }
  .branchline :global(.ic) {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    color: var(--ink-2);
  }
  .branchline .b {
    flex: 0 1 auto;
    min-width: 0;
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .branchline .from {
    flex: none;
    color: var(--ink-2);
    white-space: nowrap;
  }
  .branchline .ahead {
    margin-left: auto;
    flex: none;
    display: inline-flex;
    align-items: center;
    gap: 2px;
    font-family: var(--mono);
    font-size: var(--r0);
    font-weight: 600;
    font-variant-numeric: tabular-nums;
    color: var(--st-ok);
    cursor: default;
  }
  .branchline .ahead .arr {
    font-size: 0.8rem;
    line-height: 1;
  }
  .branchline .ahead .arr.down {
    margin-left: 4px;
  }

  /* PR chip — rectangular, hairline; the WHOLE chip is washed in the PR-state
     color (open green / merged purple / closed oxide), draft stays faint
     paper. No in-chip state word — that word lives in the title tooltip,
     which is the non-color channel. */
  .pr {
    display: inline-flex;
    align-items: center;
    gap: 7px;
    margin-top: 9px;
    font-family: var(--mono);
    font-size: var(--r0);
    color: var(--ink-1);
    background: var(--paper-2);
    border: 1px solid var(--line);
    border-radius: 0;
    padding: 3px 10px 3px 8px;
    text-decoration: none;
  }
  .pr:hover .num {
    text-decoration: underline;
  }
  .pr :global(.pric) {
    width: 13px;
    height: 13px;
    flex: 0 0 13px;
    color: currentColor;
  }
  .pr .num {
    font-weight: 600;
  }
  .pr.open {
    color: var(--st-ok);
    background: var(--st-ok-wash);
    border-color: var(--st-ok-line);
  }
  .pr.merged {
    color: var(--pr-merged);
    background: var(--pr-merged-wash);
    border-color: var(--pr-merged-line);
  }
  .pr.closed {
    color: var(--st-need);
    background: var(--st-need-wash);
    border-color: var(--st-need-line);
  }
  .pr.draft {
    color: var(--ink-2);
  }

  /* ---- dynamic git action: ONE state-derived split button --------------- */
  .actions {
    flex: none;
    padding: 12px 16px;
    border-bottom: 1px solid var(--line);
    position: relative;
  }
  .splitbtn {
    display: flex;
    align-items: stretch;
    width: 100%;
    border-radius: 0;
    overflow: hidden;
    box-shadow: 0 1px 0 oklch(100% 0 0 / 0.14) inset;
  }
  .split-main {
    flex: 1;
    min-width: 0;
    justify-content: center;
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--iris);
    color: var(--iris-on);
    border: 1px solid var(--iris-ink);
    border-right: none;
    cursor: pointer;
    white-space: nowrap;
    overflow: hidden;
    text-overflow: ellipsis;
    transition: filter 0.15s ease-out;
  }
  .split-main:hover:not(:disabled) {
    filter: brightness(1.06);
  }
  .split-main :global(.btnic) {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    color: var(--iris-on);
  }
  .split-main kbd {
    margin-left: 2px;
  }
  .split-caret {
    flex: 0 0 30px;
    display: grid;
    place-items: center;
    background: var(--iris);
    color: var(--iris-on);
    border: 1px solid var(--iris-ink);
    box-shadow: inset 1px 0 0 var(--iris-on-sc-line);
    cursor: pointer;
    transition: filter 0.15s ease-out;
  }
  .split-caret:hover {
    filter: brightness(1.06);
  }
  .split-caret :global(svg) {
    width: 13px;
    height: 13px;
    display: block;
  }
  /* Disabled (`Up to date`): quiet paper treatment, muted ink. */
  .splitbtn.disabled .split-main,
  .splitbtn.disabled .split-caret {
    background: var(--paper-2);
    color: var(--ink-3);
    border-color: var(--line);
  }
  .splitbtn.disabled .split-caret {
    box-shadow: inset 1px 0 0 var(--line);
  }
  .split-main:disabled {
    cursor: default;
  }
  .splitbtn.disabled .split-main:hover,
  .splitbtn.disabled .split-caret:hover {
    filter: none;
  }

  /* Cancel state replaces the whole split with one quiet destructive button. */
  .cancel {
    width: 100%;
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    padding: 8px 12px;
    text-align: center;
    border-radius: 0;
    color: var(--st-need);
    background: transparent;
    border: 1px solid var(--st-need-line);
    cursor: pointer;
    transition: background 0.15s ease-out;
  }
  .cancel:hover {
    background: var(--st-need-wash);
  }

  /* Quiet "why this action is primary" hint, faint mono, under the split. */
  .why-primary {
    margin-top: 8px;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-3);
    font-variant-numeric: tabular-nums;
    letter-spacing: 0.01em;
    display: flex;
    align-items: center;
    gap: 6px;
  }

  /* act-menu: the shared .menu/.mi/.m-sep recipe is global (app.css); add only
     the rail-specific width, primary marking, and disabled treatment. */
  :global(.act-menu) {
    min-width: 230px;
  }
  :global([data-theme="dark"] .act-menu) {
    box-shadow:
      0 0 0 1px oklch(8% 0.01 72 / 0.5),
      0 16px 34px -16px oklch(4% 0.01 72 / 0.7);
  }
  :global(.act-menu .mi.is-primary) {
    color: var(--iris-ink);
    font-weight: 600;
  }
  :global(.act-menu .mi.is-primary .mi-ico) {
    color: var(--iris-ink);
  }
  :global(.act-menu .mi[data-disabled]) {
    opacity: 0.42;
    pointer-events: none;
  }
  :global(.act-menu .mi[data-disabled] .mi-k) {
    color: var(--ink-3);
  }
  :global(.act-menu .mi.toggle) {
    color: var(--ink-1);
  }

  /* ---- file list -------------------------------------------------------- */
  .files {
    flex: 1;
    overflow: auto;
    min-height: 0;
    padding: 6px 10px 12px;
  }
  .empty {
    padding: 38px 20px;
    text-align: center;
  }
  .empty p {
    font-size: var(--r1);
    color: var(--ink-3);
    line-height: 1.55;
  }

  .fgroup {
    margin-top: 8px;
  }
  .fgroup h3 {
    display: flex;
    align-items: center;
    gap: 8px;
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.1em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
    padding: 6px 8px 5px;
    margin: 0;
  }
  .fgroup h3 .ct {
    color: var(--ink-3);
    font-weight: 600;
  }
  .fgroup h3 .hr {
    flex: 1;
    height: 1px;
    background: var(--line);
  }
  .fgroup h3 .all {
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--iris-ink);
    font-weight: 600;
    letter-spacing: 0.02em;
    text-transform: none;
    background: transparent;
    border: 0;
    padding: 0;
    cursor: pointer;
    transition: opacity 0.15s ease-out;
  }
  .fgroup h3 .all:hover:not(:disabled) {
    opacity: 0.75;
  }
  .fgroup h3 .all.discard-all {
    color: var(--st-need);
  }
  .fgroup h3 .all:disabled {
    opacity: 0.4;
    cursor: default;
  }

  .frow {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    text-align: left;
    font-family: var(--mono);
    font-size: var(--r1);
    padding: 5px 8px;
    border-radius: 0;
    color: var(--ink-2);
    cursor: pointer;
    background: transparent;
    border: 0;
    transition: background 0.15s ease-out;
  }
  .frow:hover {
    background: var(--paper-3);
  }
  .frow.active {
    background: var(--paper-3);
    box-shadow: inset 0 0 0 1px var(--line);
  }
  .frow .chk {
    width: 14px;
    height: 14px;
    border-radius: 0;
    flex: 0 0 14px;
    border: 1px solid var(--line);
    display: grid;
    place-items: center;
    font-size: 0.6rem;
    color: var(--paper-2);
    background: var(--paper-1);
  }
  .frow .chk.on {
    background: var(--iris);
    border-color: var(--iris-ink);
    color: var(--iris-on);
  }
  .frow .st {
    width: 13px;
    text-align: center;
    font-weight: 700;
    font-size: 0.75rem;
    flex: 0 0 13px;
  }
  .frow .st.M {
    color: var(--st-stall);
  }
  .frow .st.A {
    color: var(--st-ok);
  }
  .frow .st.D {
    color: var(--diff-del);
  }
  .frow .st.U {
    color: var(--ink-3);
  }
  .frow .path {
    flex: 1;
    min-width: 0;
    color: var(--ink-2);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .frow .path .dir {
    color: var(--ink-2);
  }
  .frow .path b {
    color: var(--ink-0);
    font-weight: 500;
  }
  .frow .fdiff {
    flex: 0 0 auto;
    margin-left: auto;
    font-size: 0.625rem;
    font-variant-numeric: tabular-nums;
  }
  .frow .fdiff .a {
    color: var(--diff-add);
  }
  /* Inline discard affordance — quiet, revealed on hover/active. */
  .frow .discard {
    flex: none;
    width: 16px;
    height: 16px;
    display: grid;
    place-items: center;
    border-radius: 0;
    color: var(--ink-3);
    opacity: 0;
    transition:
      opacity 0.15s ease-out,
      color 0.15s ease-out;
  }
  .frow:hover .discard,
  .frow.active .discard {
    opacity: 1;
  }
  .frow .discard:hover {
    color: var(--st-need);
  }

  /* ---- footer: keyboard legend ------------------------------------------ */
  .rail-r-foot {
    flex: none;
    border-top: 1px solid var(--line);
    padding: 8px 16px;
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-2);
    display: flex;
    align-items: center;
    gap: 10px;
  }
  .rail-r-foot span {
    display: inline-flex;
    align-items: center;
    gap: 5px;
  }
</style>
