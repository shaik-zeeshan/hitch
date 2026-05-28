<script lang="ts">
  // Create-worktree dialog (mockup #wt-modal). Opened from the "+" on a
  // project row and the ⌘K palette via the createWorktreeFor store (the
  // project it creates under). New-branch vs existing-branch picks the daemon's
  // WorktreeCreateMode; the base field only applies to a new branch. Throws on
  // failure surface inline; success dismisses and optionally opens a shell.
  import { Dialog, Select } from "bits-ui";
  import { createWorktree, defaultBase, listBranches, openSession } from "../daemon";
  import { createWorktreeFor } from "../overlays";
  import type { BranchSummary } from "../types";

  const project = $derived($createWorktreeFor);

  let branch = $state("");
  let mode = $state<"new-branch" | "existing-branch">("new-branch");
  let base = $state("");
  let openShell = $state(true);
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);
  let branches = $state<BranchSummary[]>([]);

  const localBranches = $derived(branches.filter((b) => !b.is_remote));
  const allBranches = $derived(branches);

  // Reset the form and fetch branches when a project first opens the dialog.
  let openedFor = $state<string | null>(null);
  $effect(() => {
    if (project && project.id !== openedFor) {
      openedFor = project.id;
      branch = "";
      mode = "new-branch";
      base = $defaultBase ?? "";
      openShell = true;
      submitting = false;
      errMsg = null;
      branches = [];
      listBranches(project.id).then((b) => {
        branches = b;
        if (!base && b.length > 0) {
          base = $defaultBase ?? b.find((x) => !x.is_remote)?.name ?? b[0].name;
        }
        if (mode === "existing-branch" && !branch && localBranches.length > 0) {
          branch = localBranches[0].name;
        }
      });
    } else if (!project) {
      openedFor = null;
    }
  });

  // When switching to existing-branch, default-select the first local branch.
  $effect(() => {
    if (mode === "existing-branch" && !branch && localBranches.length > 0) {
      branch = localBranches[0].name;
    }
  });

  function onOpenChange(next: boolean) {
    if (!next) createWorktreeFor.set(null);
  }

  async function submit() {
    const p = project;
    const name = branch.trim();
    if (!p || !name || submitting) return;
    submitting = true;
    errMsg = null;
    try {
      const created = await createWorktree(
        p.id,
        name,
        mode === "new-branch" ? base.trim() || null : null,
        mode,
      );
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
        <div class="sub">in <b>{project?.name ?? "project"}</b></div>
      </div>
      <div class="m-body">
        <div class="field">
          <span>Branch name</span>
          {#if mode === "existing-branch" && localBranches.length > 0}
            <Select.Root type="single" bind:value={branch}>
              <Select.Trigger class="select-trigger base" aria-label="Branch name">
                <Select.Value placeholder="Select a branch" />
                <span class="select-chev" aria-hidden="true">⌄</span>
              </Select.Trigger>
              <Select.Portal>
                <Select.Content class="select-content" sideOffset={6}>
                  <Select.Viewport>
                    {#each localBranches as b}
                      <Select.Item class="select-item" value={b.name} label={b.name}>
                        {b.name}
                      </Select.Item>
                    {/each}
                  </Select.Viewport>
                </Select.Content>
              </Select.Portal>
            </Select.Root>
          {:else}
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="base"
              bind:value={branch}
              placeholder="feat/my-branch"
              autofocus={mode === "new-branch"}
              onkeydown={(e) => e.key === "Enter" && void submit()}
            />
          {/if}
        </div>
        <div class="field">
          <span>Create from</span>
          <div class="seg">
            <button type="button" class:on={mode === "new-branch"} onclick={() => (mode = "new-branch")}
              >New branch</button
            >
            <button
              type="button"
              class:on={mode === "existing-branch"}
              onclick={() => (mode = "existing-branch")}>Existing branch</button
            >
          </div>
        </div>
        {#if mode === "new-branch"}
          <div class="field">
            <span>Base branch</span>
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
        {/if}
        <button type="button" class="field-row" onclick={() => (openShell = !openShell)}>
          <span class="check" class:on={openShell} aria-hidden="true">✓</span>
          <span class="lab">Open a shell session in it now</span>
        </button>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" disabled={!branch.trim() || submitting} onclick={() => void submit()}>
          {submitting ? "Creating…" : "Create worktree"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .remote-badge {
    color: var(--tx-lo);
    margin-right: 5px;
    font-size: 10px;
  }
</style>
