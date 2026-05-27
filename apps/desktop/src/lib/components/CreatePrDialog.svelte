<script lang="ts">
  // Create-PR dialog (mockup #pr-modal) — the one justified modal in the
  // design. bits-ui Dialog owns focus/escape/scroll-lock; we own the fields and
  // call createPr, which throws on failure so the error surfaces inline. On
  // success the daemon's PR url lands in the prUrl store and we show it.
  import { Dialog } from "bits-ui";
  import { createPr, defaultBase, generatePullRequestDraft, gitStatus, prUrl } from "../daemon";
  import { createPrOpen } from "../overlays";

  let { disabled = false }: { disabled?: boolean } = $props();

  let title = $state("");
  let body = $state("");
  let base = $state("");
  let draft = $state(false);
  let submitting = $state(false);
  let generating = $state(false);
  let errMsg = $state<string | null>(null);

  // Open state is shared (the ⌘K palette can open this too); reset the form on
  // each open, whether triggered here or externally.
  let wasOpen = $state(false);
  $effect(() => {
    if ($createPrOpen && !wasOpen) {
      wasOpen = true;
      title = "";
      body = "";
      base = $defaultBase ?? "";
      draft = false;
      submitting = false;
      generating = false;
      errMsg = null;
      prUrl.set(null);
    } else if (!$createPrOpen) {
      wasOpen = false;
    }
  });

  function confirmReplace(): boolean {
    if (!title.trim() && !body.trim()) return true;
    return window.confirm("Replace the current PR title and description with a generated draft?");
  }

  async function generate() {
    const targetBase = base.trim() || $defaultBase || "";
    if (!targetBase) {
      errMsg = "Enter a base branch before generating a PR draft.";
      return;
    }
    if (generating || !confirmReplace()) return;
    generating = true;
    errMsg = null;
    try {
      const draft = await generatePullRequestDraft(targetBase);
      title = draft.title;
      body = draft.body;
      base = targetBase;
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      generating = false;
    }
  }

  async function submit() {
    const t = title.trim();
    if (!t || submitting) return;
    submitting = true;
    errMsg = null;
    try {
      await createPr({
        title: t,
        body: body.trim() || null,
        base: base.trim() || null,
        draft,
      });
      // prUrl is now set; the success view replaces the form.
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }
</script>

<Dialog.Root bind:open={$createPrOpen}>
  <Dialog.Trigger class="btn grow" {disabled}>Create PR…</Dialog.Trigger>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Create pull request</Dialog.Title>
        <div class="sub">
          from <b>{$gitStatus?.branch ?? "branch"}</b> into
          <b>{base.trim() || $defaultBase || "default"}</b>
        </div>
      </div>

      {#if $prUrl}
        <div class="m-body">
          <p class="m-success">
            Pull request created.<br />
            <a href={$prUrl} target="_blank" rel="noreferrer">{$prUrl}</a>
          </p>
        </div>
        <div class="m-foot">
          <Dialog.Close class="btn primary">Done</Dialog.Close>
        </div>
      {:else}
        <div class="m-body">
          <div class="draft-actions">
            <button class="btn" disabled={generating || submitting} onclick={() => void generate()}>
              {generating ? "Generating…" : title || body ? "Regenerate" : "Generate"}
            </button>
          </div>
          <label class="field">
            <span>Title</span>
            <!-- svelte-ignore a11y_autofocus -->
            <input bind:value={title} placeholder="Pull request title" autofocus />
          </label>
          <label class="field">
            <span>Description</span>
            <textarea bind:value={body} placeholder="Add a description…"></textarea>
          </label>
          <label class="field">
            <span>Base branch</span>
            <input class="base" bind:value={base} placeholder={$defaultBase ?? "main"} />
          </label>
          <button type="button" class="field-row" onclick={() => (draft = !draft)}>
            <span class="check" class:on={draft} aria-hidden="true">✓</span>
            <span class="lab">Create as draft</span>
          </button>
          {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
        </div>
        <div class="m-foot">
          <Dialog.Close class="btn">Cancel</Dialog.Close>
          <button
            class="btn primary"
            disabled={!title.trim() || submitting}
            onclick={() => void submit()}
          >
            {submitting ? "Creating…" : "Create pull request"}
          </button>
        </div>
      {/if}
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .draft-actions {
    display: flex;
    gap: 8px;
  }
</style>
