<script lang="ts">
  // The progress toast for a remote file-drop upload (issue #31). Renders a
  // headline + percent and a Cancel button that aborts the batch before its paths
  // are inserted (per the acceptance criteria). svelte-french-toast renders a
  // custom message component with only the `toast` prop, so the payload (headline,
  // percent, batch id) rides on a custom `upload` field set by the caller via the
  // ToastOptions spread — the same pattern AppToast.svelte uses for `hitch`.
  //
  // Paper Terminal tokens only: the chip text is ink-0/mono, the Cancel button
  // borrows the app's quiet button styling (ink border, hover wash).
  import { cancelUploadBatch } from "../fileDrop";

  export type UploadToastPayload = {
    message: string;
    percent: number | null;
    batchId: string;
    // When set, the upload finished/cancelled: hide the Cancel button.
    done?: boolean;
  };

  let { toast }: { toast: { upload?: UploadToastPayload } } = $props();

  const payload = $derived(toast?.upload);

  function cancel() {
    if (payload) cancelUploadBatch(payload.batchId);
  }
</script>

{#if payload}
  <span class="up">
    <span class="msg">
      {payload.message}{#if payload.percent !== null}
        <span class="pct"> {payload.percent}%</span>
      {/if}
    </span>
    {#if !payload.done}
      <button class="cancel" type="button" onclick={cancel}>Cancel</button>
    {/if}
  </span>
{/if}

<style>
  .up {
    flex: 1 1 auto;
    display: flex;
    align-items: center;
    gap: 10px;
    font-family: var(--ui);
  }
  .msg {
    flex: 1 1 auto;
    color: var(--ink-0);
    font-weight: 600;
  }
  .pct {
    font-family: var(--mono);
    font-variant-numeric: tabular-nums;
    color: var(--ink-2);
    font-weight: 600;
  }
  .cancel {
    flex: 0 0 auto;
    font-family: var(--ui);
    font-size: 0.6875rem;
    font-weight: 600;
    color: var(--ink-1);
    background: transparent;
    border: 1px solid var(--ink-3);
    border-radius: 4px;
    padding: 2px 8px;
    cursor: pointer;
  }
  .cancel:hover {
    color: var(--ink-0);
    border-color: var(--ink-2);
    background: var(--bg-1);
  }
</style>
