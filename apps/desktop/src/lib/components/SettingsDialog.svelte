<script lang="ts">
  // Settings dialog (opened from the left-rail footer). Today it configures the
  // one preference that has UI consequences: the editor that the worktree
  // "Open in editor" action launches. The value is an OS application name (see
  // settings.ts) handed to the opener plugin, so any installed editor works by
  // name. Edits are staged locally and committed on Save so Cancel can revert.
  import { Dialog } from "bits-ui";
  import { settingsOpen } from "../overlays";
  import { DEFAULT_EDITOR, editorApp } from "../settings";

  let value = $state("");

  function onOpenChange(next: boolean) {
    settingsOpen.set(next);
    if (next) value = $editorApp;
  }

  function save() {
    editorApp.set(value.trim() || DEFAULT_EDITOR);
    settingsOpen.set(false);
  }
</script>

<Dialog.Root open={$settingsOpen} {onOpenChange}>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Settings</Dialog.Title>
      </div>
      <div class="m-body">
        <label class="field">
          <span>Editor</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input
            class="base"
            bind:value
            placeholder={DEFAULT_EDITOR}
            autofocus
            onkeydown={(e) => e.key === "Enter" && save()}
          />
        </label>
        <p class="help">
          Application used by <b>Open in editor</b> on a worktree. Any installed editor by name — e.g.
          <span class="mono">Visual Studio Code</span>, <span class="mono">Cursor</span>, <span class="mono">Zed</span>.
        </p>
      </div>
      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" onclick={() => save()}>Save</button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>
