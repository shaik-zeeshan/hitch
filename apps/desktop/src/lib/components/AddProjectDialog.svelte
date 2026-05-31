<script lang="ts">
  // Local add-project fallback dialog. The common path still goes through the
  // native folder picker (`pickAndAddProject`); this surface exists so users can
  // type or paste a path when the picker is unavailable or unsuitable.
  import { Dialog } from "bits-ui";
  import { open } from "@tauri-apps/plugin-dialog";
  import { addProject } from "../daemon";
  import { addProjectOpen } from "../overlays";

  let root = $state("");
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  function onOpenChange(next: boolean) {
    addProjectOpen.set(next);
    if (next) {
      root = "";
      submitting = false;
      errMsg = null;
    }
  }

  const canSubmit = $derived(!submitting && root.trim().length > 0);

  async function browseRoot() {
    try {
      const picked = await open({ directory: true, multiple: false, title: "Add a project folder" });
      if (typeof picked === "string") root = picked;
    } catch {
      // No picker / denied — leave the manual field available.
    }
  }

  async function submit() {
    if (!canSubmit) return;
    submitting = true;
    errMsg = null;
    try {
      await addProject(root);
      addProjectOpen.set(false);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root open={$addProjectOpen} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Add a local project</Dialog.Title>
        <div class="sub">Paste a local folder path when the native picker is unavailable.</div>
      </div>
      <div class="m-body">
        <label class="field">
          <span>Local path</span>
          <div class="input-row">
            <!-- svelte-ignore a11y_autofocus -->
            <input
              class="base"
              bind:value={root}
              placeholder="/path/to/project"
              autofocus
              onkeydown={(e) => e.key === "Enter" && void submit()}
            />
            <button type="button" class="browse" onclick={() => void browseRoot()}>Browse…</button>
          </div>
        </label>
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" disabled={!canSubmit} onclick={() => void submit()}>
          {submitting ? "Adding…" : "Add project"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
