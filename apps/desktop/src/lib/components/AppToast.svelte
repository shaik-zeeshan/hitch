<script lang="ts">
  // The custom toast message renderable for every app toast (commit/push/pull/
  // fetch). svelte-french-toast renders `toast.message` as
  // `<svelte:component this={toast.message} {toast} />`, passing ONLY the `toast`
  // prop — this version has no `props` option. So our payload rides on a custom
  // `hitch` field that the worktreeToast() wrapper (appToast.ts) sets via the
  // ToastOptions spread (the library does `...opts`, so arbitrary fields land on
  // the toast object). We read it back here.
  //
  // Two-line layout, locked to doc-design/mockup-composer.html pane 10 (.tmsg):
  //   .tsub  → main line (the headline; ink-0, weight 600)
  //   .tmeta → mono middot run under it (sha · pushed ↑N · M files · branch)
  // Segment tones map to the mockup: strong → .sha (ink-1/600), ok → .ok
  // (st-ok/600), default → plain ink-2. Separators are middots in ink-3 (.sep).
  import type { AppToastPayload } from "../appToast";

  // `toast` is svelte-french-toast's Toast object; we only need our custom
  // `hitch` field off it, so type it loosely rather than importing the lib type
  // and re-augmenting it (ToastOptions doesn't declare `hitch`).
  let { toast }: { toast: { hitch?: AppToastPayload } } = $props();

  const payload = $derived(toast?.hitch);
</script>

{#if payload}
  <span class="tmsg">
    <span class="tsub">{payload.message}</span>
    {#if payload.meta.length > 0}
      <span class="tmeta">
        {#each payload.meta as seg, i (i)}
          {#if i > 0}<span class="sep"> · </span>{/if}<span
            class:sha={seg.tone === "strong"}
            class:ok={seg.tone === "ok"}>{seg.text}</span
          >
        {/each}
      </span>
    {/if}
  </span>
{/if}

<style>
  /* Locked to mockup-composer.html .toast .tmsg block (lines ~818-837). The
     Toaster already supplies the flex row + icon; this renders the message body.
     Tokens (var()) resolve from the app root so the toast follows the
     paper/dusk theme switch. */
  .tmsg {
    flex: 1 1 auto;
    white-space: pre-line;
    font-family: var(--ui);
  }
  .tsub {
    font-weight: 600;
    color: var(--ink-0);
  }
  /* the meta line reads in mono so sha/counts/branch line up. */
  .tmeta {
    display: block;
    margin-top: 3px;
    font-family: var(--mono);
    font-size: 0.625rem;
    font-variant-numeric: tabular-nums;
    color: var(--ink-2);
  }
  .tmeta .sha {
    color: var(--ink-1);
    font-weight: 600;
  }
  .tmeta .ok {
    color: var(--st-ok);
    font-weight: 600;
  }
  .tmeta .sep {
    color: var(--ink-3);
  }
</style>
