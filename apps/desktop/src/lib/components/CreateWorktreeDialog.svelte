<script lang="ts">
  // Create-worktree dialog (mockup #wt-modal). Opened from the "+" on a
  // project row and the ⌘K palette via the createWorktreeFor store (the
  // project it creates under). New-branch vs existing-branch picks the daemon's
  // WorktreeCreateMode; the base field only applies to a new branch. Throws on
  // failure surface inline; success dismisses and optionally opens a shell.
  import { Dialog } from "bits-ui";
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
        if (mode === "existing-branch" && !branch && b.length > 0) {
          branch = b.find((x) => !x.is_remote)?.name ?? b[0].name;
        }
      });
    } else if (!project) {
      openedFor = null;
    }
  });

  // When switching to existing-branch mode, default-select first local branch.
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
        <label class="field">
          <span>Branch name</span>
          {#if mode === "existing-branch" && localBranches.length > 0}
            <select class="base" bind:value={branch}>
              {#each localBranches as b}
                <option value={b.name}>{b.name}</option>
              {/each}
            </select>
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
        </label>
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
          <label class="field">
            <span>Base branch</span>
            {#if allBranches.length > 0}
              <select class="base" bind:value={base}>
                {#each allBranches as b}
                  <option value={b.name}>{b.is_remote ? `↑ ${b.name}` : b.name}</option>
                {/each}
              </select>
            {:else}
              <input class="base" bind:value={base} placeholder={$defaultBase ?? "main"} />
            {/if}
          </label>
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
