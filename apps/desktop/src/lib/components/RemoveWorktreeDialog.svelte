<script lang="ts">
  // Remove-worktree confirmation (mockup #remove-modal). Opened from the
  // worktree context menu via the removeWorktreeTarget store. Warns about the
  // exact destructive cases (uncommitted changes, live sessions) and confirms
  // with force=true to override the daemon's guards. "Also delete the branch"
  // is gated to merged branches (ADR 0001); the frozen contract exposes no
  // merge status, so it stays disabled and delete_branch is always false.
  import { Dialog } from "bits-ui";
  import { dirtyWorktrees, removeWorktree, sessions } from "../daemon";
  import { removeWorktreeTarget } from "../overlays";

  const target = $derived($removeWorktreeTarget);
  const dirty = $derived(target ? !!$dirtyWorktrees[target.id] : false);
  const liveSessions = $derived(
    target
      ? $sessions.filter((s) => s.parent.kind === "worktree" && s.parent.id === target.id).length
      : 0,
  );

  const warning = $derived.by(() => {
    const has: string[] = [];
    if (dirty) has.push("uncommitted changes");
    if (liveSessions > 0) has.push(`${liveSessions} live session${liveSessions === 1 ? "" : "s"}`);
    if (has.length === 0) return null;
    const list = has.length === 2 ? `${has[0]} and ${has[1]}` : has[0];
    const kills = liveSessions > 0 ? " and kills the session" + (liveSessions === 1 ? "" : "s") : "";
    return `This worktree has ${list}. Removing it discards the working tree${kills}. Commits on the branch are kept.`;
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
    if (!next) removeWorktreeTarget.set(null);
  }

  async function confirm() {
    const w = target;
    if (!w || submitting) return;
    submitting = true;
    errMsg = null;
    try {
      await removeWorktree(w.id, false, true);
      removeWorktreeTarget.set(null);
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
        <Dialog.Title>Remove worktree</Dialog.Title>
        <div class="sub"><b>{target?.branch ?? ""}</b></div>
      </div>
      <div class="m-body">
        {#if warning}
          <div class="warn-box">
            <svg viewBox="0 0 16 16" fill="none" stroke="currentColor" stroke-width="1.4"
              ><path d="M8 1.8 14.8 13.5H1.2z" /><line x1="8" y1="6" x2="8" y2="9.5" /><circle
                cx="8"
                cy="11.6"
                r="0.5"
                fill="currentColor"
              /></svg
            >
            <div>{warning}</div>
          </div>
        {:else}
          <p class="help">Removing discards this worktree's checkout. Commits on the branch are kept.</p>
        {/if}
        <span class="field-row disabled">
          <span class="check" aria-hidden="true"></span>
          <span class="lab"
            >Also delete the branch <span class="mono" style="color:var(--ink-2)">(only when merged)</span></span
          >
        </span>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn danger-btn" disabled={submitting} onclick={() => void confirm()}>
          {submitting ? "Removing…" : "Remove worktree"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
