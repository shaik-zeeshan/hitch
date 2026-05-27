<script lang="ts">
  // Create-worktree dialog (mockup #wt-modal). Opened from the tree's "New
  // worktree…" row and the ⌘K palette via the createWorktreeFor store (the
  // project it creates under). New-branch vs existing-branch picks the daemon's
  // WorktreeCreateMode; the base field only applies to a new branch. Throws on
  // failure surface inline; success dismisses and optionally opens a shell.
  import { Dialog } from "bits-ui";
  import { createWorktree, defaultBase, openSession } from "../daemon";
  import { createWorktreeFor } from "../overlays";

  const project = $derived($createWorktreeFor);

  let branch = $state("");
  let mode = $state<"new-branch" | "existing-branch">("new-branch");
  let base = $state("");
  let openShell = $state(true);
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  // Reset the form when a project first opens the dialog (open is driven
  // externally, so we can't rely on onOpenChange firing for it).
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
    } else if (!project) {
      openedFor = null;
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
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="base"
            bind:value={branch}
            placeholder="feat/my-branch"
            autofocus
            onkeydown={(e) => e.key === "Enter" && void submit()}
          />
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
            <input class="base" bind:value={base} placeholder={$defaultBase ?? "main"} />
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
