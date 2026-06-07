<script lang="ts">
  // Composer — the right rail's inline card for the smart action's
  // generate-and-confirm steps (replaces the former CommitDialog modal). It is an
  // ANCHORED OVERLAY, not a modal: the action split button never leaves its slot —
  // its row morphs in place into this card's header (same iris fill/geometry, the
  // ▾ caret half becomes ×), and only the card BODY overlays the (dimmed, unmoved)
  // file list. Zero layout shift, no backdrop, Esc dismisses. Visual lock:
  // doc-design/mockup-composer.html (Button Morph). Rectangles + app tokens only.
  //
  // Slice 3 implements COMMIT mode only. PR mode is a deliberate seam: the `mode`
  // prop carries it so slice 4 can slot PR pre-flight/progress into the same shell
  // and the same header-morph recipe.
  import { tick } from "svelte";
  import { Select } from "bits-ui";
  import toast from "svelte-french-toast";
  import GitCommitHorizontal from "~icons/lucide/git-commit-horizontal";
  import GitPullRequest from "~icons/lucide/git-pull-request";
  import ArrowUp from "~icons/lucide/arrow-up";
  import {
    cancelCompositeChain,
    commit,
    compositeChainForSelectedWorktree,
    clearCompositeChain,
    defaultBase,
    generateCommitDraft,
    gitBusy,
    gitStatus,
    gitWorktreeId,
    listBranches,
    loadGitStatus,
    selectedProjectId,
    setFilesStaged,
    startCommitAndPush,
    startCreatePr,
  } from "../daemon";
  import type { BranchSummary, CompositeStep } from "../types";
  import { autoToastMessage, autoErrorMessage } from "../composerToast";
  import { commitOpen, createPrOpen } from "../overlays";
  import { currentDesktopPlatform, isShortcutModifier, shortcutKeys } from "../desktopPlatform";

  // Three modes share the `.cmp-head` morph (the constraint: reuse it, don't
  // build a parallel mechanism):
  //  - commit : glance-and-confirm (slice 3) — header morph + body overlay.
  //  - pr     : autonomous PR — pre-flight base select, then a hands-off chain
  //             whose per-step progress shows in the SAME card (steps list).
  //  - auto   : the auto-commit-push chain — header-ONLY morph (no body opens),
  //             driven entirely by the daemon's composite chain store.
  let { mode = "commit" }: { mode?: "commit" | "pr" | "auto" } = $props();

  const platform = currentDesktopPlatform();
  // The header label advertises ⏎ (no modifier) once a draft is ready: plain Enter
  // commits when focus is in the card, matching the mockup's "Commit ⏎". The
  // rail-footer / split-button advertises ⌘↵ (the global pane shortcut that opens
  // AND, while composing, also commits) — both routes are wired in onKeydown.
  const enterKeys = shortcutKeys(platform, platform === "macos" ? "⏎" : "Enter");

  // ---- commit-mode state machine ------------------------------------------
  // generating : draft Job in flight, no queue armed (hairline scans).
  // ready      : draft landed, subject/body editable (header morphs to Commit ⏎).
  // queued     : Enter pressed mid-generation → commit-on-arrival armed
  //              (header reads "Commit on ready…", hairline still scans).
  // error      : generation failed → empty editable area + retry; a fallback
  //              message is NEVER auto-committed (queue, if any, is cancelled).
  // committing : a commit request is in flight (transient).
  type Phase = "generating" | "ready" | "queued" | "error" | "committing";
  let phase = $state<Phase>("generating");
  let subject = $state("");
  let body = $state("");
  let errMsg = $state<string | null>(null);

  // Files snapshot for the scope line. We respect the currently-staged set; if
  // NOTHING is staged we auto-stage all on open, so the scope reflects that.
  const files = $derived($gitStatus?.files ?? []);
  const staged = $derived(files.filter((f) => f.staged));
  const unstaged = $derived(files.filter((f) => !f.staged));

  // The scope count is captured at open (autoStaged ? all : staged) so the footer
  // line is stable while generation/commit runs, even as a status refresh lands.
  let scopeCount = $state(0);
  let autoStaged = $state(false);

  // Bumped on each (re)generation so a draft that resolves after the user closed,
  // reopened, or retried can detect it's stale and skip clobbering fresh state —
  // mirrors CommitDialog's generationSeq guard.
  let generationSeq = 0;

  // textarea + subject element refs so the body can auto-grow and we can move
  // focus to the subject when a draft lands.
  let subjectEl = $state<HTMLInputElement | null>(null);
  let bodyEl = $state<HTMLTextAreaElement | null>(null);

  function close() {
    if (mode === "pr") {
      createPrOpen.set(false);
    } else {
      commitOpen.set(false);
    }
  }

  // Auto-grow the body textarea to its content (borderless — matches the mockup's
  // quiet auto-growing body). The CSS max-height clamps it at ~10 lines; past that
  // the height stays pinned at the cap and the textarea scrolls internally. Runs on
  // input and whenever the draft value changes programmatically.
  function autoGrow() {
    const el = bodyEl;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }

  // ---- generation ---------------------------------------------------------
  // Start (or restart) a Draft Generator run. On open we respect the staged set:
  // if anything is staged we draft from it; otherwise we stage all first, then
  // draft. Generation arrives whole (one JSON blob) — no streaming.
  async function startGeneration() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) {
      phase = "error";
      errMsg = "Select a git worktree first.";
      return;
    }
    const seq = ++generationSeq;
    const queued = phase === "queued"; // preserve an armed queue across a retry-less restart
    phase = queued ? "queued" : "generating";
    errMsg = null;
    try {
      if (autoStaged && unstaged.length > 0) {
        await setFilesStaged(
          unstaged.map((f) => f.path),
          true,
          worktreeId,
        );
        if (seq !== generationSeq) return;
      }
      const draft = await generateCommitDraft(worktreeId);
      if (seq !== generationSeq) return;
      subject = draft.subject;
      body = draft.body;
      // If the user armed commit-on-arrival while we were generating, fire now.
      if (phase === "queued") {
        await submit();
        return;
      }
      phase = "ready";
      await tick();
      autoGrow();
      subjectEl?.focus();
      subjectEl?.select();
    } catch (err) {
      if (seq !== generationSeq) return;
      // Generation FAILED. Cancel any armed queue (never auto-commit a fallback),
      // surface the error with an EMPTY editable area + retry.
      subject = "";
      body = "";
      errMsg = err instanceof Error ? err.message : String(err);
      phase = "error";
      await tick();
      subjectEl?.focus();
    }
  }

  function regenerate() {
    if (phase === "committing") return;
    subject = "";
    body = "";
    void startGeneration();
  }

  // ---- commit -------------------------------------------------------------
  async function submit() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    if (!subject.trim()) {
      // Nothing to commit yet (error state with empty subject) — keep the card
      // open for manual typing rather than firing an empty commit.
      subjectEl?.focus();
      return;
    }
    phase = "committing";
    errMsg = null;
    try {
      await commit(subject, body, worktreeId);
      close();
    } catch (err) {
      errMsg = err instanceof Error ? err.message : String(err);
      // A commit failure drops back to an editable ready state so the user can
      // fix the message (or staging) and retry with Enter.
      phase = "ready";
    }
  }

  // Enter (plain, focus in card) or Cmd/Ctrl+Enter (anywhere) confirms. While
  // generating, the same key ARMS commit-on-arrival instead. Esc dismisses, or —
  // while queued — cancels the queue back to generating.
  function attemptConfirm() {
    if (phase === "committing") return;
    if (phase === "generating") {
      // Queue commit-on-arrival.
      phase = "queued";
      return;
    }
    if (phase === "queued") return; // already armed
    // ready or error: commit if there's a subject (error keeps card open otherwise).
    void submit();
  }

  function onKeydown(event: KeyboardEvent) {
    if (mode === "pr") {
      if (event.key === "Escape") {
        event.preventDefault();
        event.stopPropagation();
        // Esc only dismisses BEFORE confirm; after confirm the chain is daemon-
        // owned and Esc does nothing (the body reads "hands-off"). A parked
        // failure can still be dismissed.
        if (!prInProgress || prFailed) close();
        return;
      }
      if (event.key === "Enter" && !prInProgress) {
        event.preventDefault();
        event.stopPropagation();
        void confirmPr();
      }
      return;
    }
    if (event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      if (phase === "queued") {
        // Esc cancels the armed queue and returns to plain generating.
        phase = "generating";
        return;
      }
      close();
      return;
    }
    if (event.key === "Enter") {
      const mod = isShortcutModifier(event, platform);
      const target = event.target;
      const inBody = target === bodyEl;
      // In the body textarea a bare Enter inserts a newline; only Cmd/Ctrl+Enter
      // commits from there. In the subject (single line) or on the card chrome a
      // bare Enter also confirms. Cmd/Ctrl+Enter always confirms.
      if (mod || !inBody) {
        event.preventDefault();
        event.stopPropagation();
        attemptConfirm();
      }
    }
  }

  // ---- open lifecycle -----------------------------------------------------
  // On open: capture the staging decision + scope, then kick generation. We read
  // the live status once at open; the staged/unstaged derivations stay reactive
  // for display but the open-time snapshot drives the auto-stage rule.
  let wasOpen = false;
  $effect(() => {
    if (mode !== "commit") return;
    if ($commitOpen && !wasOpen) {
      wasOpen = true;
      const stagedNow = staged.length;
      autoStaged = stagedNow === 0;
      scopeCount = autoStaged ? files.length : stagedNow;
      subject = "";
      body = "";
      errMsg = null;
      phase = "generating";
      void startGeneration();
    } else if (!$commitOpen && wasOpen) {
      wasOpen = false;
      // Invalidate any in-flight generation so a late draft can't clobber a
      // re-opened session.
      generationSeq += 1;
    }
  });

  // Move focus into the card when it opens so Enter/Esc are caught immediately.
  let cardEl = $state<HTMLElement | null>(null);
  $effect(() => {
    const open = mode === "pr" ? $createPrOpen : $commitOpen;
    if (open && cardEl) {
      void tick().then(() => {
        // Focus the subject if it exists yet (commit ready/error), else the card
        // root so the keydown handler still receives Enter/Esc.
        (subjectEl ?? cardEl)?.focus();
      });
    }
  });

  const scopeLabel = $derived(
    `${scopeCount} ${autoStaged ? "" : "staged "}file${scopeCount === 1 ? "" : "s"}`.replace("  ", " "),
  );

  // ---- shared chain state (PR + auto modes) -------------------------------
  // The daemon-owned composite chain for the selected worktree drives both the
  // PR step progress and the auto header morph. It is restored from active-Jobs
  // on attach (daemon.ts), so leave-and-return mid-chain is exact.
  const chain = $derived($compositeChainForSelectedWorktree);

  // Human label for a chain step in the morphed header (auto mode + PR head).
  function stepLabel(step: CompositeStep): string {
    switch (step) {
      case "staging":
        return "staging…";
      case "drafting":
        return "drafting message…";
      case "committing":
        return "committing…";
      case "pushing":
        return "pushing…";
      case "creating-pr":
        return "creating PR…";
    }
  }

  // ---- PR pre-flight + autonomous chain -----------------------------------
  // PR mode opens to a base-branch select (prefilled with the default base). On
  // confirm the `create-pr` chain runs hands-off; the card stays as a step
  // progress display. Title/body are NEVER reviewed locally. Esc before confirm
  // dismisses; after confirm the chain is daemon-owned (Esc/leaving does not
  // cancel it — the body reads "hands-off").
  let prBase = $state("");
  let prBranches = $state<BranchSummary[]>([]);
  let prConfirmed = $state(false);
  let prErr = $state<string | null>(null);

  // The PR chain's live step from the shared store (only while this worktree's
  // chain is a create-pr). Drives the header label + the steps list highlight.
  const prChain = $derived(chain?.kind === "create-pr" ? chain : null);
  const prFailed = $derived(prChain?.failed ?? null);

  // PR is "in progress" once confirmed locally OR the daemon reports a create-pr
  // chain for this worktree (restored after navigation). Pre-flight shows until
  // then.
  const prInProgress = $derived(prConfirmed || Boolean(prChain && !prFailed));

  // Reset + load branches when PR mode opens.
  let prWasOpen = false;
  $effect(() => {
    if (mode !== "pr") return;
    if ($createPrOpen && !prWasOpen) {
      prWasOpen = true;
      prErr = null;
      // If a create-pr chain is already running for this worktree (restored from
      // active-Jobs), skip the pre-flight and show progress directly.
      prConfirmed = Boolean(prChain && !prFailed);
      prBase = $defaultBase ?? "";
      const projectId = $selectedProjectId;
      if (projectId) {
        void listBranches(projectId)
          .then((b) => {
            prBranches = b;
            if (!prBase && b.length > 0) {
              prBase = $defaultBase ?? b.find((x) => !x.is_remote)?.name ?? b[0].name;
            }
          })
          .catch(() => {});
      }
    } else if (!$createPrOpen && prWasOpen) {
      prWasOpen = false;
      prConfirmed = false;
    }
  });

  // The PR steps list (mockup #pr-progress). `done`/`now`/`todo` derive from the
  // live chain step. Ordered: pushing → drafting → creating-pr → open browser.
  type PrStep = { key: CompositeStep | "open"; label: string };
  const PR_STEPS: PrStep[] = [
    { key: "pushing", label: "push" },
    { key: "drafting", label: "draft PR" },
    { key: "creating-pr", label: "create PR" },
    { key: "open", label: "open in browser" },
  ];
  const PR_ORDER: (CompositeStep | "open")[] = ["pushing", "drafting", "creating-pr", "open"];
  function prStepState(key: CompositeStep | "open"): "done" | "now" | "todo" {
    const current = prChain?.step ?? "pushing";
    const ci = PR_ORDER.indexOf(current);
    const ki = PR_ORDER.indexOf(key);
    if (ki < ci) return "done";
    if (ki === ci) return "now";
    return "todo";
  }

  async function confirmPr() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId || prConfirmed) return;
    const base = prBase.trim() || $defaultBase || null;
    prConfirmed = true;
    prErr = null;
    try {
      // The chain runs hands-off; the daemon layer opens the created draft PR in
      // the browser on completion (only while attached) — both for this confirmed
      // path and a chain restored after navigating away, with one open guard.
      await startCreatePr(base, worktreeId);
      // Close the card; the rail's PR chip now reflects the created PR.
      createPrOpen.set(false);
      prConfirmed = false;
    } catch (err) {
      // The chain failed mid-flight. The shared store has the oxide failure (the
      // header shows ✗ <step> failed — retry); keep the card open with the reason.
      prErr = err instanceof Error ? err.message : String(err);
      prConfirmed = false;
    }
  }

  // ---- auto chain (commit-and-push) ---------------------------------------
  // Auto mode renders header-only (no body). The chain is started by RightRail's
  // toggle handler; this component just reflects the shared store + offers retry.
  const autoChain = $derived(chain?.kind === "commit-and-push" ? chain : null);
  const autoFailed = $derived(autoChain?.failed ?? null);

  // The morphed auto label per step, with live counts where the mockup shows them
  // (staging 4 files…, pushing ↑1…). Counts come from the live status snapshot.
  const autoStepLabel = $derived.by(() => {
    const step = autoChain?.step ?? "staging";
    if (step === "staging") {
      const n = files.length;
      return `staging ${n} file${n === 1 ? "" : "s"}…`;
    }
    if (step === "drafting") return "drafting message…";
    if (step === "committing") return "committing…";
    if (step === "pushing") {
      const ahead = $gitStatus?.ahead ?? 0;
      return ahead > 0 ? `pushing ↑${ahead}…` : "pushing…";
    }
    return stepLabel(step);
  });

  // Retry the auto chain after a failure (re-runs the whole chain). Same handler
  // shape RightRail uses, but kept here so the failed button's retry caret works
  // from the morphed header.
  async function retryAuto() {
    const worktreeId = $gitWorktreeId;
    if (!worktreeId) return;
    clearCompositeChain(worktreeId);
    const id = toast.loading("Staging files…");
    try {
      const result = await startCommitAndPush(worktreeId);
      toast.success(autoToastMessage(result), { id });
    } catch (err) {
      toast.error(autoErrorMessage(err), { id });
    }
  }
</script>

{#if $commitOpen && mode === "commit"}
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="composer"
    class:is-error={phase === "error"}
    bind:this={cardEl}
    role="dialog"
    aria-label="Commit composer"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <!-- The morphed split-button row = the card header, kept in the button's slot.
         Its label morphs per phase; the close half is the old caret → ×. -->
    <div class="cmp-head" class:queued={phase === "queued"}>
      {#if phase === "generating"}
        <span class="label on-iris"><span class="spin"></span> Drafting commit…</span>
      {:else if phase === "queued"}
        <span class="label on-iris"><span class="spin"></span> Commit on ready…</span>
      {:else}
        <span class="label on-iris">
          <GitCommitHorizontal class="btnic icon" />
          Commit
          <span class="keys">{#each enterKeys as k (k)}<kbd>{k}</kbd>{/each}</span>
        </span>
      {/if}
      <button
        class="close"
        type="button"
        title={phase === "queued" ? "Cancel queue" : "Close"}
        aria-label={phase === "queued" ? "Cancel commit-on-ready" : "Close composer"}
        onclick={() => {
          if (phase === "queued") {
            phase = "generating";
          } else {
            close();
          }
        }}
      >×</button>
      {#if phase === "generating" || phase === "queued" || phase === "error"}
        <div class="hairline"></div>
      {/if}
    </div>

    <!-- The card BODY — the only overlaying element. Absolutely positioned beneath
         the in-flow header; extends over the dimmed file list. -->
    <div class="cmp-body">
      {#if phase === "error" && errMsg}
        <p class="cmp-error">{errMsg}</p>
      {/if}

      <input
        class="cmp-subject"
        bind:this={subjectEl}
        bind:value={subject}
        placeholder={phase === "error" ? "type a subject…" : "subject — generating…"}
        readonly={phase === "committing"}
        spellcheck="false"
      />
      <textarea
        class="cmp-bodytext"
        bind:this={bodyEl}
        bind:value={body}
        rows="2"
        placeholder={phase === "error" ? "type a body (optional)…" : "body — generating…"}
        readonly={phase === "committing"}
        spellcheck="false"
        oninput={autoGrow}
      ></textarea>

      <div class="cmp-foot">
        {#if phase === "queued"}
          <span class="scope" title="will commit on ready · {scopeLabel}">will commit on ready · <b>{scopeLabel}</b></span>
        {:else}
          <span class="scope" title="committing {scopeLabel}">committing <b>{scopeLabel}</b></span>
        {/if}
        <span class="spacer"></span>
        {#if phase === "ready"}
          <button class="act" type="button" disabled={$gitBusy} onclick={regenerate}>↻ regenerate</button>
        {:else if phase === "error"}
          <button class="act" type="button" disabled={$gitBusy} onclick={regenerate}>↻ retry draft</button>
        {/if}
        {#if phase === "queued"}
          <span class="esc"><kbd>esc</kbd> cancel queue</span>
        {:else if phase === "generating"}
          <span class="esc"><kbd>esc</kbd> cancel</span>
        {:else}
          <span class="esc"><kbd>esc</kbd></span>
        {/if}
      </div>
    </div>
  </div>
{/if}

{#if mode === "pr" && ($createPrOpen || prChain || prFailed)}
  <!-- PR mode: pre-flight (base select) → hands-off chain (step progress). The
       header reuses .cmp-head; the body holds either the pre-flight row or the
       steps list. Title/body are never reviewed locally. -->
  <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
  <!-- svelte-ignore a11y_no_noninteractive_tabindex -->
  <div
    class="composer"
    class:is-error={Boolean(prFailed)}
    bind:this={cardEl}
    role="dialog"
    aria-label="Create pull request composer"
    tabindex="-1"
    onkeydown={onKeydown}
  >
    <div class="cmp-head" class:is-error={Boolean(prFailed)}>
      {#if prFailed}
        <span class="label"><span class="err-mk">✗</span> {prFailed.step} failed — retry</span>
      {:else if prInProgress}
        <span class="label on-iris">
          <ArrowUp class="btnic icon" />
          {stepLabel(prChain?.step ?? "pushing")}
        </span>
      {:else}
        <span class="label on-iris">
          <GitPullRequest class="btnic icon" />
          Create draft PR
          <span class="keys">{#each enterKeys as k (k)}<kbd>{k}</kbd>{/each}</span>
        </span>
      {/if}
      <button
        class="close"
        type="button"
        title={prFailed ? "Retry" : prInProgress ? "Hands-off" : "Close"}
        aria-label={prFailed ? "Retry create pull request" : "Close composer"}
        disabled={prInProgress && !prFailed}
        onclick={() => {
          if (prFailed) {
            void confirmPr();
          } else if (!prInProgress) {
            close();
          }
        }}
      >{prFailed ? "▾" : "×"}</button>
      {#if prInProgress && !prFailed}
        <div class="hairline"></div>
      {/if}
    </div>

    {#if prFailed}
      <div class="auto-reason">! {prFailed.reason}</div>
    {:else}
      <div class="cmp-body">
        {#if prInProgress}
          <ul class="cmp-steps">
            {#each PR_STEPS as s (s.key)}
              {@const st = prStepState(s.key)}
              <li class={st}>
                <span class="mk">
                  {#if st === "done"}✓{:else if st === "now"}<span class="spin2"></span>{:else}·{/if}
                </span>
                {s.label}
              </li>
            {/each}
          </ul>
          <div class="cmp-foot">
            <span class="scope">creating a GitHub <b>draft PR</b></span>
            <span class="spacer"></span>
            <span class="esc">hands-off</span>
          </div>
        {:else}
          {#if prErr}<p class="cmp-error">{prErr}</p>{/if}
          <div class="cmp-prerow">
            <span class="lab">base branch</span>
            {#if prBranches.length > 0}
              <Select.Root type="single" bind:value={prBase}>
                <Select.Trigger class="select-trigger" aria-label="Base branch">
                  <Select.Value placeholder={$defaultBase ?? "main"} />
                  <span class="select-chev" aria-hidden="true">⌄</span>
                </Select.Trigger>
                <Select.Portal>
                  <Select.Content class="select-content" sideOffset={6}>
                    <Select.Viewport>
                      {#each prBranches as b (b.name)}
                        <Select.Item
                          class="select-item"
                          value={b.name}
                          label={b.is_remote ? `↑ ${b.name}` : b.name}
                        >
                          {#if b.is_remote}<span class="remote-badge">↑</span>{/if}{b.name}
                        </Select.Item>
                      {/each}
                    </Select.Viewport>
                  </Select.Content>
                </Select.Portal>
              </Select.Root>
            {:else}
              <input class="cmp-baseinput" bind:value={prBase} placeholder={$defaultBase ?? "main"} spellcheck="false" />
            {/if}
          </div>
          <div class="cmp-foot">
            <span class="scope">drafts a GitHub <b>draft PR</b></span>
            <span class="spacer"></span>
            <span class="esc"><kbd>esc</kbd></span>
          </div>
        {/if}
      </div>
    {/if}
  </div>
{/if}

{#if mode === "auto" && autoChain}
  <!-- Auto mode: header-ONLY morph (no body opens; the file list stays fully
       visible & undimmed). Reflects the daemon-owned chain store, so a leave-and-
       return restores the exact step. × cancels the remaining steps; on failure
       the header goes oxide with ✗ <step> failed — retry ▾ + one reason line. -->
  <div class="composer auto">
    <div class="cmp-head" class:is-error={Boolean(autoFailed)}>
      {#if autoFailed}
        <span class="label"><span class="err-mk">✗</span> {autoFailed.step} failed — retry</span>
        <button class="close" type="button" title="Retry" aria-label="Retry commit and push" onclick={() => void retryAuto()}>▾</button>
      {:else}
        <span class="label on-iris"><span class="iris-dot"></span> {autoStepLabel}</span>
        <button
          class="close"
          type="button"
          title="Cancel chain"
          aria-label="Cancel commit and push"
          onclick={() => void cancelCompositeChain()}
        >×</button>
        <div class="hairline"></div>
      {/if}
    </div>
  </div>
  {#if autoFailed}
    <div class="auto-reason">! {autoFailed.reason}</div>
  {/if}
{/if}

<style>
  /* The in-flow composer wrapper. Sits exactly where the .splitbtn sat (RightRail
     renders it in the same .actions slot), and is the positioning context for the
     absolute body. z-index lifts the card over the dimmed file list. */
  .composer {
    position: relative;
    z-index: 30;
    outline: none;
  }

  /* The morphed button row = the card header, KEPT IN FLOW in the button slot.
     Same iris fill/geometry/inset highlight as the .splitbtn it replaces; a split
     into a morphing label half + a close (×) half (the old caret). */
  .cmp-head {
    position: relative;
    z-index: 2; /* above the body's top border so the seam reads as one edge */
    display: flex;
    align-items: stretch;
    width: 100%;
    border: 1px solid var(--iris-ink);
    border-radius: 0;
    overflow: hidden;
    box-shadow: 0 1px 0 oklch(100% 0 0 / 0.14) inset;
  }
  .cmp-head .label {
    flex: 1;
    min-width: 0;
    justify-content: center;
    display: inline-flex;
    align-items: center;
    gap: 8px;
    padding: 8px 12px;
    background: var(--iris);
    color: var(--iris-on);
    font-family: var(--ui);
    font-size: var(--r1);
    font-weight: 600;
    white-space: nowrap;
  }
  .cmp-head .label :global(.btnic) {
    width: 14px;
    height: 14px;
    flex: 0 0 14px;
    color: var(--iris-on);
  }
  .cmp-head .label .keys {
    margin-left: 2px;
  }
  /* the close half — the old ▾ caret morphs into the dismiss affordance (×). */
  .cmp-head .close {
    flex: 0 0 30px;
    display: grid;
    place-items: center;
    background: var(--iris);
    color: var(--iris-on);
    border: 0;
    box-shadow: inset 1px 0 0 var(--iris-on-sc-line);
    cursor: pointer;
    font-family: var(--mono);
    font-size: 14px;
    line-height: 1;
    transition: filter 0.15s ease-out;
  }
  .cmp-head .close:hover {
    filter: brightness(1.06);
  }
  /* a tiny inline spinner glyph that rides in the morphed label */
  .cmp-head .spin {
    width: 11px;
    height: 11px;
    border: 1.5px solid var(--iris-on-sc-line);
    border-top-color: var(--iris-on);
    border-radius: 50%;
    animation: cmpSpin 0.8s linear infinite;
    flex: 0 0 11px;
  }
  @keyframes cmpSpin {
    to {
      transform: rotate(360deg);
    }
  }
  /* a queued header is quieter — label reads "Commit on ready…" but stays on the
     iris fill so it still reads as the same morphing button. */
  .cmp-head.queued .label {
    color: var(--iris-on-sc);
  }

  /* the 1px hairline progress edge — sits at the SEAM (bottom edge of the header
     row) so it reads as part of the unified card, not floating above it. */
  .cmp-head .hairline {
    position: absolute;
    left: 0;
    right: 0;
    bottom: -1px;
    height: 1px;
    overflow: hidden;
    z-index: 3;
  }
  .cmp-head .hairline::before {
    content: "";
    position: absolute;
    top: 0;
    height: 1px;
    width: 38%;
    left: -38%;
    background: var(--iris);
    animation: cmpScan 1.15s ease-in-out infinite;
  }
  @keyframes cmpScan {
    0% {
      left: -38%;
    }
    100% {
      left: 100%;
    }
  }

  /* the card BODY — the ONLY overlaying element. Absolutely positioned directly
     beneath the in-flow header row; extends downward over the dimmed file list.
     Its top border tucks under the header's bottom so the two read as one
     continuous rectangle (shared iris-ink frame, no seam but the hairline). */
  .cmp-body {
    position: absolute;
    left: 0;
    right: 0;
    top: 100%;
    margin-top: -1px;
    background: var(--paper-2);
    border: 1px solid var(--iris-ink);
    border-top: none;
    border-radius: 0;
    box-shadow: var(--shadow-pop);
    padding: 10px 12px 9px;
  }

  /* error swaps the scanning hairline for a solid oxide edge + recolors the
     unified card frame (header + body) to the oxide attention color. */
  .composer.is-error .cmp-head,
  .composer.is-error .cmp-body {
    border-color: var(--st-need-line);
  }
  .composer.is-error .cmp-head .hairline {
    height: 1px;
    background: var(--st-need);
  }
  .composer.is-error .cmp-head .hairline::before {
    display: none;
  }

  /* subject: styled like an input ONLY on focus/hover; borderless at rest. */
  .cmp-subject {
    width: 100%;
    display: block;
    font-family: var(--mono);
    font-size: var(--r1);
    font-weight: 600;
    color: var(--ink-0);
    background: transparent;
    border: 1px solid transparent;
    border-radius: 0;
    padding: 4px 5px;
    margin: 0 -5px;
    outline: none;
  }
  .cmp-subject::placeholder {
    color: var(--ink-3);
    font-weight: 500;
  }
  .cmp-subject:hover {
    border-color: var(--line);
  }
  .cmp-subject:focus {
    border-color: var(--iris);
    background: var(--paper-3);
  }

  /* body: quiet auto-growing textarea; borderless at rest like the subject.
     auto-grows via autoGrow() up to ~10 text lines (line-height 1.55 × var(--r1))
     plus the 8px vertical padding, then clamps and scrolls inside the card so a
     long draft can't push the overlay past the file list. */
  .cmp-bodytext {
    width: 100%;
    display: block;
    font-family: var(--mono);
    font-size: var(--r1);
    line-height: 1.55;
    color: var(--ink-1);
    background: transparent;
    border: 1px solid transparent;
    border-radius: 0;
    padding: 4px 5px;
    margin: 4px -5px 0;
    max-height: calc(10 * 1.55 * var(--r1) + 8px);
    resize: none;
    outline: none;
    overflow-x: hidden;
    overflow-y: auto;
    /* thin neutral scrollbar, matching the global convention in app.css. */
    scrollbar-width: thin;
    scrollbar-color: var(--ink-3) transparent;
  }
  .cmp-bodytext::placeholder {
    color: var(--ink-3);
  }
  .cmp-bodytext:hover {
    border-color: var(--line);
  }
  .cmp-bodytext:focus {
    border-color: var(--iris);
    background: var(--paper-3);
  }

  /* the single muted footer line: scope on the left, regenerate/esc on the right.
     it must never wrap — the scope truncates while the controls stay intact. */
  .cmp-foot {
    display: flex;
    flex-wrap: nowrap;
    align-items: center;
    gap: 8px;
    margin-top: 7px;
    padding-top: 7px;
    border-top: 1px solid var(--line);
    font-family: var(--mono);
    font-size: 0.625rem;
    color: var(--ink-3);
  }
  /* the scope is the one thing that gives way: it shrinks and ellipsises. */
  .cmp-foot .scope {
    flex: 0 1 auto;
    min-width: 0;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    color: var(--ink-2);
  }
  .cmp-foot .scope b {
    color: var(--ink-1);
    font-weight: 600;
  }
  .cmp-foot .spacer {
    flex: 1;
  }
  /* the right-side controls never shrink or wrap — they stay fully visible. */
  .cmp-foot .act {
    flex: none;
    white-space: nowrap;
    color: var(--iris-ink);
    cursor: pointer;
    display: inline-flex;
    align-items: center;
    gap: 4px;
    font: inherit;
    background: transparent;
    border: 0;
    padding: 0;
    transition: opacity 0.15s ease-out;
  }
  .cmp-foot .act:hover:not(:disabled) {
    opacity: 0.75;
  }
  .cmp-foot .act:disabled {
    opacity: 0.4;
    cursor: default;
  }
  .cmp-foot .esc {
    flex: none;
    white-space: nowrap;
    color: var(--ink-3);
    display: inline-flex;
    align-items: center;
    gap: 4px;
  }
  .cmp-foot .esc kbd {
    translate: 0 0;
  }

  /* error line inside the body */
  .cmp-error {
    font-family: var(--mono);
    font-size: var(--r0);
    color: var(--st-need);
    line-height: 1.5;
    margin: 0 0 8px;
    word-break: break-word;
  }

  /* ---- PR pre-flight: a base-branch select row -------------------------- */
  .cmp-prerow {
    margin-bottom: 2px;
  }
  .cmp-prerow .lab {
    display: block;
    font-family: var(--mono);
    font-size: 0.625rem;
    letter-spacing: 0.06em;
    text-transform: uppercase;
    color: var(--ink-2);
    font-weight: 700;
    margin: 0 0 6px;
  }
  /* fallback text input when no branches are listed (mirrors .select-trigger) */
  .cmp-baseinput {
    width: 100%;
    min-height: 34px;
    display: block;
    background: var(--paper-3);
    border: 1px solid var(--line);
    border-radius: 0;
    color: var(--ink-0);
    font-family: var(--mono);
    font-size: var(--r1);
    padding: 8px 10px;
    outline: none;
  }
  .cmp-baseinput:focus {
    border-color: var(--iris);
  }
  .remote-badge {
    color: var(--ink-2);
    margin-right: 5px;
    font-size: 10px;
  }

  /* ---- PR progress steps shown in the body (action stays in the label) --- */
  .cmp-steps {
    list-style: none;
    margin: 2px 0 4px;
    padding: 0;
    font-family: var(--mono);
    font-size: var(--r1);
    color: var(--ink-2);
  }
  .cmp-steps li {
    display: flex;
    align-items: center;
    gap: 8px;
    padding: 3px 0;
  }
  .cmp-steps li .mk {
    width: 13px;
    text-align: center;
    flex: 0 0 13px;
    font-size: 0.75rem;
  }
  .cmp-steps li.done {
    color: var(--ink-1);
  }
  .cmp-steps li.done .mk {
    color: var(--st-ok);
  }
  .cmp-steps li.now {
    color: var(--ink-0);
  }
  .cmp-steps li.now .mk {
    color: var(--iris-ink);
  }
  .cmp-steps li.now .mk .spin2 {
    display: inline-block;
    width: 10px;
    height: 10px;
    border: 1.5px solid var(--iris-line);
    border-top-color: var(--iris-ink);
    border-radius: 50%;
    animation: cmpSpin 0.8s linear infinite;
    vertical-align: -1px;
  }
  .cmp-steps li.todo {
    color: var(--ink-3);
  }

  /* ---- AUTO MODE — header-only morph ------------------------------------ */
  /* a small dotted-ring glyph that rides at the head of an auto label */
  .cmp-head .iris-dot {
    flex: 0 0 13px;
    width: 13px;
    height: 13px;
    border-radius: 50%;
    border: 1.5px dashed var(--iris-on-sc-line);
    color: var(--iris-on);
    animation: cmpSpin 2.4s linear infinite;
  }
  /* failed-chain head: oxide attention treatment (matches the manual error
     frame), label reads ✗ <step> failed — retry ▾, with a retry caret half. */
  .cmp-head.is-error {
    border-color: var(--st-need-line);
  }
  .cmp-head.is-error .label {
    background: var(--st-need-wash);
    color: var(--st-need);
  }
  .cmp-head.is-error .close {
    background: var(--st-need-wash);
    color: var(--st-need);
    box-shadow: inset 1px 0 0 var(--st-need-line);
    font-family: var(--ui);
    font-size: 12px;
  }
  .cmp-head.is-error .label .err-mk {
    font-size: 13px;
    line-height: 1;
  }

  /* a single muted reason line shown directly under a failed header (header-
     only — no card body opens for the auto chain; PR failure reuses it too). */
  .auto-reason {
    position: relative;
    z-index: 30;
    margin-top: 8px;
    font-family: var(--mono);
    font-size: 0.625rem;
    line-height: 1.5;
    color: var(--ink-3);
    word-break: break-word;
  }

  @media (prefers-reduced-motion: reduce) {
    .cmp-head .spin,
    .cmp-head .iris-dot,
    .cmp-steps li.now .mk .spin2,
    .cmp-head .hairline::before {
      animation-duration: 0.001ms;
      animation-iteration-count: 1;
    }
  }
</style>
