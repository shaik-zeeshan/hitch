<script lang="ts">
  // Commit overlay — editable subject/body plus explicit draft generation.
  // Drafts are local to the overlay and never auto-generate on open.
  import { Dialog } from "bits-ui";
  import {
    commit,
    generateCommitDraft,
    gitBusy,
    gitStatus,
    loadGitStatus,
    setFilesStaged,
  } from "../daemon";
  import { commitOpen } from "../overlays";

  let { disabled = false, triggerClass = "btn primary full" }: { disabled?: boolean; triggerClass?: string } = $props();

  let subject = $state("");
  let body = $state("");
  let submitting = $state(false);
  let generating = $state(false);
  let errMsg = $state<string | null>(null);

  const files = $derived($gitStatus?.files ?? []);
  const staged = $derived(files.filter((file) => file.staged));
  const unstaged = $derived(files.filter((file) => !file.staged));
  const canCommit = $derived(staged.length > 0 && subject.trim().length > 0 && !$gitBusy && !submitting);
  const canGenerate = $derived(staged.length > 0 && !$gitBusy && !generating);
  const canStageAllAndGenerate = $derived(staged.length === 0 && unstaged.length > 0 && !$gitBusy && !generating);

  // Bumped on each open-reset so an in-flight draft request that resolves after
  // the dialog was closed and reopened can detect it's stale and skip clobbering
  // the freshly-reset blank form (or flipping `generating` for the new session).
  let generationSeq = 0;

  let wasOpen = $state(false);
  $effect(() => {
    if ($commitOpen && !wasOpen) {
      wasOpen = true;
      generationSeq += 1;
      subject = "";
      body = "";
      submitting = false;
      generating = false;
      errMsg = null;
    } else if (!$commitOpen) {
      wasOpen = false;
    }
  });

  function confirmReplace(): boolean {
    if (!subject.trim() && !body.trim()) return true;
    return window.confirm("Replace the current commit text with a generated draft?");
  }

  async function generate() {
    if (!canGenerate || !confirmReplace()) return;
    const seq = generationSeq;
    generating = true;
    errMsg = null;
    try {
      const draft = await generateCommitDraft();
      if (seq !== generationSeq) return;
      subject = draft.subject;
      body = draft.body;
    } catch (err) {
      if (seq !== generationSeq) return;
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      if (seq === generationSeq) generating = false;
    }
  }

  async function stageAllAndGenerate() {
    if (!canStageAllAndGenerate || !confirmReplace()) return;
    const seq = generationSeq;
    generating = true;
    errMsg = null;
    try {
      const paths = unstaged.map((file) => file.path);
      await setFilesStaged(paths, true);
      const draft = await generateCommitDraft();
      if (seq !== generationSeq) return;
      subject = draft.subject;
      body = draft.body;
      if ($gitStatus?.worktree_id) void loadGitStatus($gitStatus.worktree_id).catch(() => {});
    } catch (err) {
      if (seq !== generationSeq) return;
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      if (seq === generationSeq) generating = false;
    }
  }

  async function submit() {
    if (!canCommit) return;
    submitting = true;
    errMsg = null;
    try {
      await commit(subject, body);
      commitOpen.set(false);
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
    } finally {
      submitting = false;
    }
  }

  function onSubjectKey(event: KeyboardEvent) {
    if (event.key === "Enter" && (event.metaKey || event.ctrlKey)) {
      event.preventDefault();
      if (canCommit) void submit();
    }
  }
</script>

<Dialog.Root bind:open={$commitOpen}>
  <Dialog.Trigger class={triggerClass} {disabled}>Commit…</Dialog.Trigger>
  <Dialog.Portal>
    <Dialog.Overlay class="modal-back" />
    <Dialog.Content class="modal" aria-describedby={undefined}>
      <div class="m-head">
        <Dialog.Title>Commit staged changes</Dialog.Title>
        <div class="sub">
          {staged.length > 0
            ? `${staged.length} staged file${staged.length === 1 ? "" : "s"}`
            : "No staged files"}
        </div>
      </div>

      <div class="m-body">
        <div class="draft-actions">
          <button class="btn" disabled={!canGenerate} onclick={() => void generate()}>
            {generating && staged.length > 0 ? "Generating…" : subject || body ? "Regenerate" : "Generate"}
          </button>
          {#if canStageAllAndGenerate}
            <button class="btn" onclick={() => void stageAllAndGenerate()}>
              {generating ? "Staging…" : "Stage all & generate"}
            </button>
          {/if}
        </div>

        <label class="field">
          <span>Subject</span>
          <!-- svelte-ignore a11y_autofocus -->
          <input bind:value={subject} placeholder="chore: update files" autofocus onkeydown={onSubjectKey} />
        </label>
        <label class="field">
          <span>Body</span>
          <textarea bind:value={body} placeholder="Optional commit body…"></textarea>
        </label>
        {#if staged.length === 0}
          <p class="m-error">Stage at least one file before committing.</p>
        {/if}
        {#if errMsg}<p class="m-error">{errMsg}</p>{/if}
      </div>

      <div class="m-foot">
        <Dialog.Close class="btn">Cancel</Dialog.Close>
        <button class="btn primary" disabled={!canCommit} onclick={() => void submit()}>
          {submitting ? "Committing…" : "Commit"}
          <span class="kbd">⌘⏎</span>
        </button>
      </div>
    </Dialog.Content>
  </Dialog.Portal>
</Dialog.Root>

<style>
  .draft-actions {
    display: flex;
    gap: 8px;
  }
</style>
