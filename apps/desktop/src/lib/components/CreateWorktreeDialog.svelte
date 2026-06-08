<script lang="ts">
  // Create-worktree dialog. Opened from the "+" on a project row and the
  // command palette via the createWorktreeFor store (the project it creates under).
  //
  // There is no "new branch / existing branch" mode toggle. A single search
  // field drives a palette-style list: the top row always offers to CREATE a
  // branch named after what you typed (off the chosen base), and below it the
  // project's existing local branches (check one out) and remote branches
  // (create a local branch tracking it) filter as you type. Hitch infers the
  // git operation from which row you pick — you never name the distinction.
  // Throws surface inline; success dismisses and optionally opens a shell.
  import { Dialog, Select } from "bits-ui";
  import {
    createWorktree,
    defaultBase,
    listBranches,
    openSession,
    scopeAttributionForProject,
  } from "../daemon";
  import { createWorktreeFor } from "../overlays";
  import { localBranchNameForRemote, remoteBranchChoices } from "../branchChoices";
  import type { BranchSummary } from "../types";

  const project = $derived($createWorktreeFor);
  // The project is already chosen, so the target daemon scope is a DISPLAY here
  // (not a select like CloneProjectDialog): a remote project shows its SSH Host so
  // the user knows the worktree is created on that machine (issue #30, ADR 0014).
  // list-branches + create-worktree both route to this owning scope inside
  // daemon.ts. Local shows no scope line (clean local copy).
  const attribution = $derived(scopeAttributionForProject(project?.id));

  type Row =
    | { kind: "create"; key: string; name: string }
    | { kind: "existing"; key: string; name: string }
    | { kind: "remote"; key: string; name: string; localName: string };

  const ADJECTIVES = [
    "sunny", "calm", "brave", "fuzzy", "swift", "quiet", "bold", "silly",
    "crisp", "lucky", "wild", "amber", "dusty", "frozen", "gentle", "hollow",
  ];
  const NOUNS = [
    "toast", "river", "maple", "cedar", "pebble", "flame", "ridge", "cloud",
    "cabin", "dune", "ember", "grove", "haven", "inlet", "lemon", "orbit",
  ];

  function pick<T>(arr: T[]): T {
    return arr[Math.floor(Math.random() * arr.length)];
  }

  function generateBranchName(): string {
    const n = Math.floor(Math.random() * 90) + 10;
    return `${pick(ADJECTIVES)}-${pick(NOUNS)}-${n}`;
  }

  let query = $state("");
  let base = $state("");
  let openShell = $state(true);
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);
  let branches = $state<BranchSummary[]>([]);
  let generatedPlaceholder = $state(generateBranchName());
  let activeIndex = $state(0);

  const q = $derived(query.trim());
  const ql = $derived(q.toLowerCase());
  const localBranches = $derived(branches.filter((b) => !b.is_remote));
  const localBranchNames = $derived(new Set(localBranches.map((b) => b.name)));
  const remoteBranches = $derived(remoteBranchChoices(branches));
  const localMatches = $derived(
    q ? localBranches.filter((b) => b.name.toLowerCase().includes(ql)) : localBranches,
  );
  const remoteMatches = $derived(
    q ? remoteBranches.filter((b) => b.name.toLowerCase().includes(ql)) : remoteBranches,
  );
  // Hide the create row only when the text is already an exact local branch
  // (creating a duplicate would fail; picking that branch checks it out).
  const exactLocal = $derived(localBranchNames.has(q));
  const createName = $derived(q || generatedPlaceholder);

  const rows = $derived.by<Row[]>(() => {
    const out: Row[] = [];
    if (!exactLocal) out.push({ kind: "create", key: "create", name: createName });
    for (const b of localMatches) out.push({ kind: "existing", key: `l:${b.name}`, name: b.name });
    for (const b of remoteMatches) {
      const localName = localBranchNameForRemote(b.name);
      out.push({ kind: "remote", key: `r:${b.name}`, name: b.name, localName });
    }
    return out;
  });

  const allBranches = $derived(branches);
  const activeRow = $derived(rows[activeIndex] ?? null);
  const primaryLabel = $derived(
    submitting
      ? "Creating…"
      : activeRow?.kind === "existing"
        ? "Check out branch"
        : activeRow?.kind === "remote"
          ? "Create from remote"
          : "Create branch",
  );

  // Typing always re-aims the selection at the top (the create row), so Enter
  // creates exactly what you typed without an extra keystroke.
  $effect(() => {
    query;
    activeIndex = 0;
  });
  // Keep the cursor in range as the list shrinks.
  $effect(() => {
    if (activeIndex >= rows.length) activeIndex = Math.max(0, rows.length - 1);
  });

  // Reset the form and fetch branches when a project first opens the dialog.
  let openedFor = $state<string | null>(null);
  $effect(() => {
    if (project && project.id !== openedFor) {
      openedFor = project.id;
      query = "";
      generatedPlaceholder = generateBranchName();
      base = $defaultBase ?? "";
      openShell = true;
      submitting = false;
      errMsg = null;
      branches = [];
      activeIndex = 0;
      listBranches(project.id).then((b) => {
        branches = b;
        if (!base && b.length > 0) {
          base = $defaultBase ?? b.find((x) => !x.is_remote)?.name ?? b[0].name;
        }
      });
    } else if (!project) {
      openedFor = null;
    }
  });

  function onOpenChange(next: boolean) {
    if (!next) createWorktreeFor.set(null);
  }

  function onKey(event: KeyboardEvent) {
    if (event.key === "ArrowDown") {
      event.preventDefault();
      activeIndex = Math.min(activeIndex + 1, rows.length - 1);
    } else if (event.key === "ArrowUp") {
      event.preventDefault();
      activeIndex = Math.max(activeIndex - 1, 0);
    } else if (event.key === "Enter") {
      event.preventDefault();
      if (activeRow) void submit(activeRow);
    }
  }

  async function submit(row: Row) {
    const p = project;
    if (!p || submitting) return;
    let branchName: string;
    let baseRef: string | null;
    let mode: "new-branch" | "existing-branch";
    if (row.kind === "existing") {
      branchName = row.name;
      baseRef = null;
      mode = "existing-branch";
    } else if (row.kind === "remote") {
      branchName = row.localName;
      baseRef = row.name;
      mode = "new-branch";
    } else {
      branchName = row.name.trim();
      baseRef = base.trim() || null;
      mode = "new-branch";
    }
    if (!branchName) return;
    submitting = true;
    errMsg = null;
    try {
      const created = await createWorktree(p.id, branchName, baseRef, mode);
      if (created && openShell) {
        await openSession({ kind: "worktree", id: created.id }, "shell", null);
      }
      createWorktreeFor.set(null);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root open={project !== null} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>New worktree</Dialog.Title>
        <div class="sub">
          in <b>{project?.name ?? "project"}</b>{#if attribution.isRemote}
            <span class="on-host"> on <b>{attribution.label}</b></span>{/if}
        </div>
      </div>
      <div class="m-body">
        <div class="field">
          <span>Search or name a branch</span>
          <div class="wt-search">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
              ><circle cx="7" cy="7" r="4.5" /><line x1="10.5" y1="10.5" x2="14" y2="14" /></svg
            >
            <!-- svelte-ignore a11y_autofocus -->
            <input
              bind:value={query}
              placeholder={generatedPlaceholder}
              autofocus
              spellcheck="false"
              autocomplete="off"
              onkeydown={onKey}
            />
          </div>
        </div>

        <div class="wt-list" role="listbox" tabindex="-1" aria-label="Branches">
          {#each rows as row, i (row.key)}
            <button
              type="button"
              class="wt-row"
              class:active={i === activeIndex}
              role="option"
              aria-selected={i === activeIndex}
              onmousemove={() => (activeIndex = i)}
              onclick={() => void submit(row)}
            >
              {#if row.kind === "create"}
                <svg class="wt-ico create" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.5" stroke-linecap="round"
                  ><path d="M8 3.5v9M3.5 8h9" /></svg
                >
                <span class="wt-lab">Create branch <span class="mono">{row.name}</span></span>
                <span class="wt-hint">from {base || $defaultBase || "base"}</span>
              {:else if row.kind === "existing"}
                <svg class="wt-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3" stroke-linecap="round" stroke-linejoin="round"
                  ><circle cx="4" cy="3.5" r="1.6" /><circle cx="4" cy="12.5" r="1.6" /><circle cx="12" cy="5" r="1.6" /><path
                    d="M4 5.1v5.8M12 6.6C12 9.8 8.8 11 4.6 11"
                  /></svg
                >
                <span class="wt-lab"><span class="mono">{row.name}</span></span>
                <span class="wt-hint">check out</span>
              {:else}
                <svg class="wt-ico" viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.3"
                  ><path d="M8 13V5M4.5 8.5 8 5l3.5 3.5M3 3.5h10" /></svg
                >
                <span class="wt-lab"><span class="mono">{row.name}</span></span>
                <span class="wt-hint">new local branch</span>
              {/if}
            </button>
          {/each}
          {#if rows.length === 0}
            <p class="wt-empty">No branches yet — type a name to create one.</p>
          {/if}
        </div>

        <div class="field">
          <span>Base for new branch</span>
          {#if allBranches.length > 0}
            <Select.Root type="single" bind:value={base}>
              <Select.Trigger class="select-trigger base" aria-label="Base branch">
                <Select.Value placeholder={$defaultBase ?? "main"} />
                <span class="select-chev" aria-hidden="true">⌄</span>
              </Select.Trigger>
              <Select.Portal>
                <Select.Content class="select-content" sideOffset={6}>
                  <Select.Viewport>
                    {#each allBranches as b}
                      <Select.Item
                        class="select-item"
                        value={b.name}
                        label={b.is_remote ? `↑ ${b.name}` : b.name}
                      >
                        {#if b.is_remote}<span class="remote-badge">↑</span>{/if}{b.name}
                      </Select.Item>
                    {/each}
                  </Select.Viewport>
                </Select.Content>
              </Select.Portal>
            </Select.Root>
          {:else}
            <input class="base" bind:value={base} placeholder={$defaultBase ?? "main"} />
          {/if}
        </div>

        <button type="button" class="field-row" onclick={() => (openShell = !openShell)}>
          <span class="check" class:on={openShell} aria-hidden="true">✓</span>
          <span class="lab">Open a shell session in it now</span>
        </button>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button
          class="btn primary"
          disabled={!activeRow || submitting}
          onclick={() => activeRow && void submit(activeRow)}
        >
          {primaryLabel}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .remote-badge {
    color: var(--ink-2);
    margin-right: 5px;
    font-size: 10px;
  }

  /* Search field — borrows the palette input feel inside the dialog. */
  .wt-search {
    display: flex;
    align-items: center;
    gap: 9px;
    background: var(--paper-3);
    border: 1px solid var(--line);
    border-radius: 0;
    padding: 0 10px;
    transition: border-color 0.18s ease-out;
  }
  .wt-search:focus-within {
    border-color: var(--iris);
  }
  .wt-search svg {
    width: 14px;
    height: 14px;
    color: var(--ink-2);
    flex: none;
  }
  .wt-search input {
    flex: 1;
    min-width: 0;
    background: transparent;
    border: 0;
    outline: 0;
    color: var(--ink-0);
    font: inherit;
    font-size: 12.5px;
    padding: 8px 0;
  }
  .wt-search input::placeholder {
    color: var(--ink-2);
  }

  /* Results list — a permanent panel in the dialog body, not a popover. */
  .wt-list {
    max-height: 188px;
    overflow-y: auto;
    background: var(--paper-3);
    border: 1px solid var(--line);
    border-radius: 0;
    padding: 4px;
    display: grid;
    gap: 1px;
  }
  .wt-row {
    display: flex;
    align-items: center;
    gap: 9px;
    width: 100%;
    text-align: left;
    font: inherit;
    padding: 7px 8px;
    border: 0;
    border-radius: 0;
    background: transparent;
    color: var(--ink-1);
    cursor: pointer;
    min-width: 0;
  }
  .wt-row.active {
    background: var(--iris-wash);
    color: var(--ink-0);
    box-shadow: inset 0 0 0 1px var(--iris-line);
  }
  .wt-ico {
    width: 15px;
    height: 15px;
    color: var(--ink-2);
    flex: none;
  }
  .wt-ico.create {
    color: var(--iris-ink);
  }
  .wt-row.active .wt-ico {
    color: var(--iris-ink);
  }
  .wt-lab {
    flex: 1;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    font-size: 12px;
  }
  .wt-lab .mono {
    font-family: var(--mono);
    font-size: 11.5px;
  }
  .wt-hint {
    flex: none;
    font-size: 10px;
    color: var(--ink-2);
    white-space: nowrap;
  }
  .wt-empty {
    padding: 14px 10px;
    text-align: center;
    font-size: 11.5px;
    color: var(--ink-2);
  }
</style>
