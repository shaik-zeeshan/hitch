<script lang="ts">
  // Remove-project confirmation. This forgets the project in Hitch without
  // deleting the user's project root (or worktree folders) from disk. Live
  // sessions under the project are killed after explicit confirmation.
  import { Dialog } from "bits-ui";
  import { removeProject, sessions, worktrees } from "../daemon";
  import { removeProjectTarget } from "../overlays";

  const target = $derived($removeProjectTarget);
  const projectWorktrees = $derived(
    target ? $worktrees.filter((worktree) => worktree.project_id === target.id) : [],
  );
  const projectWorktreeIds = $derived(new Set(projectWorktrees.map((worktree) => worktree.id)));
  const liveSessions = $derived(
    target
      ? $sessions.filter(
          (session) =>
            (session.parent.kind === "project" && session.parent.id === target.id) ||
            (session.parent.kind === "worktree" && projectWorktreeIds.has(session.parent.id)),
        ).length
      : 0,
  );

  const summary = $derived.by(() => {
    const bits: string[] = [];
    if (projectWorktrees.length > 0) {
      bits.push(`${projectWorktrees.length} worktree${projectWorktrees.length === 1 ? "" : "s"}`);
    }
    if (liveSessions > 0) {
      bits.push(`${liveSessions} live session${liveSessions === 1 ? "" : "s"}`);
    }
    return bits.length > 0 ? bits.join(" · ") : "No worktrees or live sessions";
  });

  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  let openedFor = $state<string | null>(null);
  $effect(() => {
    if (target && target.id !== openedFor) {
      openedFor = target.id;
      submitting = false;
      errMsg = null;
    } else if (!target) {
      openedFor = null;
    }
  });

  function onOpenChange(next: boolean) {
    if (!next) removeProjectTarget.set(null);
  }

  async function confirm() {
    const project = target;
    if (!project || submitting) return;
    submitting = true;
    errMsg = null;
    try {
      await removeProject(project.id, true);
      removeProjectTarget.set(null);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root open={target !== null} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Remove project</Dialog.Title>
        <div class="sub"><b>{target?.name ?? ""}</b></div>
      </div>
      <div class="m-body">
        {#if liveSessions > 0}
          <div class="warn-box">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
              ><path d="M8 1.8 14.8 13.5H1.2z" /><line x1="8" y1="6" x2="8" y2="9.5" /><circle
                cx="8"
                cy="11.6"
                r="0.5"
                fill="currentColor"
              /></svg
            >
            <div>Removing this project kills {liveSessions} live session{liveSessions === 1 ? "" : "s"}.</div>
          </div>
        {/if}
        <p class="help">
          This removes the project from Hitch only. Files at
          <span class="mono">{target?.root ?? ""}</span> and any worktree folders stay on disk.
        </p>
        <p class="help">{summary}</p>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn danger-btn" disabled={submitting} onclick={() => void confirm()}>
          {submitting ? "Removing…" : "Remove project"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
