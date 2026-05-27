<script lang="ts">
  // Add-project dialog (mockup #proj-modal). Local folder maps to the daemon's
  // add-project (it detects git vs plain); Clone remote maps to clone-project.
  // Paths can be typed or picked: the "Browse…" buttons open a native directory
  // picker via the dialog plugin and write the result into the bound field.
  // Throws surface inline; success dismisses.
  import { Dialog } from "bits-ui";
  import { open } from "@tauri-apps/plugin-dialog";
  import { addProject, cloneProject } from "../daemon";
  import { addProjectOpen } from "../overlays";

  let tab = $state<"local" | "clone">("local");
  let folder = $state("");
  let remoteUrl = $state("");
  let destination = $state("");
  let submitting = $state(false);
  let errMsg = $state<string | null>(null);

  function onOpenChange(next: boolean) {
    addProjectOpen.set(next);
    if (next) {
      tab = "local";
      folder = "";
      remoteUrl = "";
      destination = "";
      submitting = false;
      errMsg = null;
    }
  }

  const canSubmit = $derived(
    submitting
      ? false
      : tab === "local"
        ? folder.trim().length > 0
        : remoteUrl.trim().length > 0 && destination.trim().length > 0,
  );

  // Native directory picker; returns the chosen path or null (cancelled/denied).
  async function pickFolder(): Promise<string | null> {
    try {
      const picked = await open({ directory: true, multiple: false });
      return typeof picked === "string" ? picked : null;
    } catch {
      return null;
    }
  }

  async function browseFolder() {
    const picked = await pickFolder();
    if (picked) folder = picked;
  }

  async function browseDestination() {
    const picked = await pickFolder();
    if (picked) destination = picked;
  }

  async function submit() {
    if (!canSubmit) return;
    submitting = true;
    errMsg = null;
    try {
      if (tab === "local") {
        await addProject(folder);
      } else {
        await cloneProject(remoteUrl, destination);
      }
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
        <Dialog.Title>Add project</Dialog.Title>
      </div>
      <div class="m-body">
        <div class="seg">
          <button type="button" class:on={tab === "local"} onclick={() => (tab = "local")}>Local folder</button>
          <button type="button" class:on={tab === "clone"} onclick={() => (tab = "clone")}>Clone remote</button>
        </div>

        {#if tab === "local"}
          <label class="field">
            <span>Folder</span>
            <div class="input-row">
              <!-- svelte-ignore a11y_autofocus -->
              <input
                class="base"
                bind:value={folder}
                placeholder="/path/to/repo"
                autofocus
                onkeydown={(e) => e.key === "Enter" && void submit()}
              />
              <button type="button" class="browse" onclick={() => void browseFolder()}>Browse…</button>
            </div>
          </label>
          <p class="help">
            Hitch detects whether the folder is a git repo (worktrees enabled) or a plain folder
            (terminals only).
          </p>
        {:else}
          <label class="field">
            <span>Remote URL</span>
            <input class="base" bind:value={remoteUrl} placeholder="https://github.com/owner/repo.git" />
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
          <p class="help">Clones the remote into that location, then adds it as a project.</p>
        {/if}
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" disabled={!canSubmit} onclick={() => void submit()}>
          {submitting ? (tab === "clone" ? "Cloning…" : "Adding…") : tab === "clone" ? "Clone & add" : "Add project"}
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
