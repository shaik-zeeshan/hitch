<script lang="ts">
  // Remove-worktree confirmation (mockup #remove-modal). Opened from the
  // worktree context menu via the removeWorktreeTarget store. Warns about the
  // exact destructive cases (uncommitted changes, live sessions) and confirms
  // with force=true to override the daemon's guards. "Also delete the branch"
  // is gated to merged branches (ADR 0001); the frozen contract exposes no
  // merge status, so it stays disabled and delete_branch is always false.
  import { Dialog } from "bits-ui";
  import { dirtyWorktrees, removeWorktree, scopeAttributionForWorktree, sessions } from "../daemon";
  import { removeWorktreeTarget } from "../overlays";
  import { removeWorktreeTitle, remotePathAttribution } from "../scopeCopy";
  import { worktreeToast } from "../appToast";
  import { autoErrorMessage, logAutoError } from "../composerToast";

  const target = $derived($removeWorktreeTarget);
  const dirty = $derived(target ? !!$dirtyWorktrees[target.id] : false);
  // Remote attribution (issue #30, ADR 0014): a remote worktree's confirmation
  // titles `Remove worktree on <host>?` and shows the SSH Host + remote path so a
  // path that also exists locally can't be mistaken for local state. Local copy is
  // unchanged. `$worktrees` (read inside scopeAttributionForWorktree) keeps this
  // reactive to scope tagging.
  const attribution = $derived(scopeAttributionForWorktree(target?.id));
  const title = $derived(removeWorktreeTitle(attribution));
  const remoteLine = $derived(target ? remotePathAttribution(attribution, target.path) : null);
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

  function onOpenChange(next: boolean) {
    if (!next) removeWorktreeTarget.set(null);
  }

  // Removal is a non-blocking background task surfaced via a toast (loading →
  // success/error), matching every other git action (see RightRail). Bind the
  // toast to the worktree's identity BEFORE removal: worktreeToast() resolves the
  // branch label at bind time, and removeWorktree() prunes the worktree from the
  // store, so binding later would lose the label. The dialog closes immediately
  // so the (possibly slow, remote-over-SSH) removal never blocks the user.
  function confirm() {
    const w = target;
    if (!w) return;
    const t = worktreeToast(w.id);
    const id = t.loading("Removing worktree…");
    removeWorktreeTarget.set(null);
    void removeWorktree(w.id, false, true)
      .then(() => t.success("Worktree removed", { id }))
      .catch((err) => {
        logAutoError("remove worktree", err);
        t.error(autoErrorMessage(err), { id });
      });
  }
</script>

<Dialog.Root open={target !== null} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>{title}</Dialog.Title>
        <div class="sub"><b>{target?.branch ?? ""}</b></div>
        {#if remoteLine}
          <div class="sub mono remote-attr">{remoteLine}</div>
        {/if}
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
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn danger-btn" onclick={confirm}>Remove worktree</button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
