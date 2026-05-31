<script lang="ts">
  // Clone-remote dialog. Adding a LOCAL project no longer uses a dialog — it
  // opens the native folder picker directly (pickAndAddProject); this surface
  // is only the remote-clone path, opened from the ⌘K palette and the left-rail
  // Add-project menu. Clone maps to the daemon's clone-project Job. The
  // "Browse…" button opens a native directory picker for the clone destination.
  // Throws surface inline; success dismisses.
  import { Dialog } from "bits-ui";
  import { open } from "@tauri-apps/plugin-dialog";
  import { cloneProject } from "../daemon";
  import { cloneProjectOpen } from "../overlays";

  let remoteUrl = $state("");
  let destination = $state("");
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  function onOpenChange(next: boolean) {
    cloneProjectOpen.set(next);
    if (next) {
      remoteUrl = "";
      destination = "";
      submitting = false;
      errMsg = null;
    }
  }

  const canSubmit = $derived(
    !submitting && remoteUrl.trim().length > 0 && destination.trim().length > 0,
  );

  // Native directory picker for the clone destination; cancelled/denied is a no-op.
  async function browseDestination() {
    try {
      const picked = await open({ directory: true, multiple: false });
      if (typeof picked === "string") destination = picked;
    } catch {
      // No picker / denied — leave the field as-is.
    }
  }

  async function submit() {
    if (!canSubmit) return;
    submitting = true;
    errMsg = null;
    try {
      await cloneProject(remoteUrl, destination);
      cloneProjectOpen.set(false);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root open={$cloneProjectOpen} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Clone a repository</Dialog.Title>
        <div class="sub">Clone a remote, then add it as a project.</div>
      </div>
      <div class="m-body">
        <label class="field">
          <span>Remote URL</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="base"
            bind:value={remoteUrl}
            placeholder="https://github.com/owner/repo.git"
            autofocus
            onkeydown={(e) => e.key === "Enter" && void submit()}
          />
        </label>
        <label class="field">
          <span>Clone into</span>
          <div class="input-row">
            <input
              class="base"
              bind:value={destination}
              placeholder="/path/to/clone-here"
              onkeydown={(e) => e.key === "Enter" && void submit()}
            />
            <button type="button" class="browse" onclick={() => void browseDestination()}>Browse…</button>
          </div>
        </label>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" disabled={!canSubmit} onclick={() => void submit()}>
          {submitting ? "Cloning…" : "Clone & add"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
