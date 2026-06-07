<script lang="ts">
  // Remove SSH Host confirmation (issue #26, ADR 0014). Opened from the SSH Host
  // scope row's context menu via the removeSshHostTarget store. Removing forgets
  // ONLY the GUI-local host entry — it does not (and in this slice cannot) shut
  // down any remote Daemon or kill remote Sessions. Mirrors RemoveWorktreeDialog's
  // confirm vocabulary (.modal/.btn.danger-btn).
  import { Dialog } from "bits-ui";
  import { removeSshHost } from "../sshHosts";
  import { forgetRemoteScope } from "../daemon";
  import { removeSshHostTarget } from "../overlays";

  const target = $derived($removeSshHostTarget);

  function onOpenChange(next: boolean) {
    if (!next) removeSshHostTarget.set(null);
  }

  function confirm() {
    const host = target;
    if (!host) return;
    // Prune the host's GUI-local entities/sessions/jobs FIRST (while its scope
    // tags still resolve), then forget the saved attachment — which drops its
    // tree scope row and, via the sshHosts subscription, disconnects the proxy.
    // The remote Daemon + its Sessions keep running (ADR 0014).
    forgetRemoteScope(host.id);
    removeSshHost(host.id);
    removeSshHostTarget.set(null);
  }
</script>

<Dialog.Root open={target !== null} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Remove SSH Host</Dialog.Title>
        <div class="sub"><b>{target?.target ?? ""}</b></div>
      </div>
      <div class="m-body">
        <p class="help">
          This forgets the saved host on this machine only. It does not touch the
          remote daemon or any sessions running on the host.
        </p>
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn danger-btn" onclick={() => confirm()}>Remove host</button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
