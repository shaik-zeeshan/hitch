<script lang="ts">
  // Clone-remote dialog. Adding a LOCAL project no longer uses a dialog — it
  // opens the native folder picker directly (pickAndAddProject); this surface
  // is only the remote-clone path, opened from the command palette and the left-rail
  // Add-project menu. Clone maps to the daemon's clone-project Job. The
  // "Browse…" button opens a native directory picker for the clone destination.
  // Throws surface inline; success dismisses.
  import { Dialog } from "bits-ui";
  import { open } from "@tauri-apps/plugin-dialog";
  import { get } from "svelte/store";
  import { cloneProject, daemonScopesOrdered, selectedScopeId } from "../daemon";
  import { cloneProjectOpen } from "../overlays";
  import { LOCAL_SCOPE_ID, type DaemonScopeId } from "../types";

  let remoteUrl = $state("");
  let destination = $state("");
  let scopeId = $state<DaemonScopeId>(LOCAL_SCOPE_ID);
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  // Target-daemon select (issue #28, ADR 0014): Local + connected hosts, default
  // to the currently selected scope. Clone routes to that daemon; when remote, the
  // destination is a remote path (text only — no native picker for remote paths).
  const scopes = $derived($daemonScopesOrdered);
  const isLocal = $derived(scopeId === LOCAL_SCOPE_ID);

  function onOpenChange(next: boolean) {
    cloneProjectOpen.set(next);
    if (next) {
      remoteUrl = "";
      destination = "";
      scopeId = get(selectedScopeId);
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
      await cloneProject(remoteUrl, destination, null, scopeId);
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
          <span>Target daemon</span>
          <select class="base" bind:value={scopeId}>
            {#each scopes as scope (scope.id)}
              <option value={scope.id}>{scope.label}</option>
            {/each}
          </select>
        </label>
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
          <span>Clone into{#if !isLocal} (remote path){/if}</span>
          <div class="input-row">
            <input
              class="base"
              bind:value={destination}
              placeholder={isLocal ? "/path/to/clone-here" : "/absolute/remote/path"}
              onkeydown={(e) => e.key === "Enter" && void submit()}
            />
            <!-- Native picker only resolves LOCAL paths; a remote destination is
                 typed (the GUI never maps remote paths onto local paths). -->
            {#if isLocal}
              <button type="button" class="browse" onclick={() => void browseDestination()}>Browse…</button>
            {/if}
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
